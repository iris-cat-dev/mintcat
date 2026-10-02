use std::time::Duration;

use anyhow::Result;
use clap::ArgMatches;
use mintcat_sdk::{JobEvent, JobEventPage, JobSnapshot, JobState};
use serde_json::{json, Value};
use tokio::time::{self, Instant};

use super::{application_error, emit, failure, Connection, Failure};

pub async fn follow(connection: &mut Connection, job_id: &str, cursor: u64, matches: &ArgMatches, compact: bool) -> Result<()> {
    follow_initial(connection, job_id, cursor, matches, compact, None).await
}

pub async fn follow_initial(connection: &mut Connection, job_id: &str, mut cursor: u64, matches: &ArgMatches, compact: bool, mut initial: Option<(Value, u64)>) -> Result<()> {
    let events = matches.get_flag("events") || initial.is_some();
    let limit = initial.as_ref().map(|(_, limit)| *limit).unwrap_or(100);
    let deadline = Instant::now() + Duration::from_secs(*matches.get_one::<u64>("wait-timeout").unwrap());
    let poll = Duration::from_millis(*matches.get_one::<u64>("poll-ms").unwrap());
    loop {
        if Instant::now() >= deadline { return Err(timeout(job_id, cursor)); }
        let page = match initial.take() {
            Some((value, _)) => value,
            None => match time::timeout_at(deadline, connection.request("jobs.watch", json!({"jobId":job_id,"afterSeq":cursor,"limit":limit}), None)).await {
                Ok(Ok(value)) => value,
                Ok(Err(error)) if error.downcast_ref::<Failure>().is_some_and(|failure| matches!(failure.code.as_str(), "TRANSPORT_ERROR" | "DEADLINE_EXCEEDED")) => {
                    eprintln!("mintcat: reconnecting job {job_id} after cursor {cursor}: {error}");
                    time::sleep_until((Instant::now() + poll).min(deadline)).await;
                    continue;
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => { connection.client = None; return Err(timeout(job_id, cursor)); }
            },
        };
        let page: JobEventPage = serde_json::from_value(page)
            .map_err(|error| failure("INVALID_RESULT", json!({"method":"jobs.watch","message":error.to_string()})))?;
        validate_page(&page, job_id, cursor)?;
        if page.reset_required && events {
            emit(&json!({"type":"reset","retentionStartSeq":page.retention_start_seq,"snapshot":page.snapshot}), true)?;
        }
        // The cursor is durable per job, not per IPC generation.
        for event in new_events(&page, cursor) {
            if events { emit(&json!({"type":"event","event":event}), true)?; }
            cursor = event.seq;
        }
        cursor = cursor.max(page.next_after_seq);
        if stop_state(&page.snapshot.state) && cursor >= page.snapshot.last_seq {
            emit(&json!({"type":"snapshot","snapshot":page.snapshot,"afterSeq":cursor}), compact || events)?;
            return final_status(&page.snapshot);
        }
        // Drain backlog without waiting, otherwise respect the user-selected polling interval.
        if cursor >= page.snapshot.last_seq {
            time::sleep_until((Instant::now() + poll).min(deadline)).await;
        }
    }
}
fn new_events(page: &JobEventPage, after_seq: u64) -> impl Iterator<Item = &JobEvent> {
    page.events.iter().filter(move |event| event.seq > after_seq)
}
fn validate_page(page: &JobEventPage, job_id: &str, cursor: u64) -> Result<()> {
    let mut last = None;
    if page.snapshot.job_id != job_id || page.next_after_seq < cursor || page.next_after_seq > page.snapshot.last_seq {
        return Err(failure("INVALID_RESPONSE", json!({"message":"Job watch snapshot/cursor mismatch","jobId":job_id})));
    }
    for event in &page.events {
        if event.job_id != job_id || event.seq > page.snapshot.last_seq || last.is_some_and(|previous| event.seq <= previous) {
            return Err(failure("INVALID_RESPONSE", json!({"message":"Job watch event identity/sequence mismatch","jobId":job_id})));
        }
        last = Some(event.seq);
    }
    if last.is_some_and(|seq| page.next_after_seq < seq) {
        return Err(failure("INVALID_RESPONSE", json!({"message":"Job watch cursor precedes its events","jobId":job_id})));
    }
    if page.next_after_seq == cursor && cursor < page.snapshot.last_seq {
        return Err(failure("INVALID_RESPONSE", json!({"message":"Job watch page did not advance through its advertised backlog","jobId":job_id,"afterSeq":cursor})));
    }
    Ok(())
}
fn stop_state(state: &JobState) -> bool {
    matches!(state, JobState::Succeeded | JobState::Failed | JobState::Cancelled | JobState::RecoveryRequired | JobState::AwaitingApproval)
}
fn final_status(snapshot: &JobSnapshot) -> Result<()> {
    match snapshot.state {
        JobState::Succeeded => Ok(()),
        JobState::Failed => match snapshot.error.clone() {
            Some(error) => Err(application_error(error)),
            None => Err(failure("JOB_FAILED", json!({"jobId":snapshot.job_id}))),
        },
        JobState::Cancelled => Err(failure("JOB_CANCELLED", json!({"jobId":snapshot.job_id}))),
        JobState::RecoveryRequired => Err(failure("RECOVERY_REQUIRED", json!({"jobId":snapshot.job_id,"snapshot":snapshot}))),
        JobState::AwaitingApproval => Err(failure("ACTION_REQUIRED", json!({"jobId":snapshot.job_id,"snapshot":snapshot,"message":"Look up the durable action; explicitly approve its exact revision/response. No UI is needed for approved service commands; Nexus/browser interactions remain explicit."}))),
        _ => unreachable!("final_status is only called for stopping states"),
    }
}
fn timeout(job_id: &str, cursor: u64) -> anyhow::Error {
    failure("WAIT_TIMEOUT", json!({"jobId":job_id,"afterSeq":cursor,"message":"Job remains durable and is not cancelled","recovery":{"command":"jobs watch","arguments":{"jobId":job_id,"afterSeq":cursor}}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page() -> JobEventPage {
        serde_json::from_value(json!({
            "snapshot":{"jobId":"job-a","command":"install.plan","state":"running","generationId":"new","lastSeq":5,"createdAt":"now","updatedAt":"now"},
            "events":[{"jobId":"job-a","seq":4,"generationId":"old","phase":"run","state":"running"},{"jobId":"job-a","seq":5,"generationId":"new","phase":"run","state":"running"}],
            "nextAfterSeq":5,"retentionStartSeq":1,"resetRequired":false
        })).unwrap()
    }
    #[test]
    fn reconnect_accepts_prior_generation_replay_but_rejects_cross_job_and_regressing_cursor() {
        let page = page();
        validate_page(&page, "job-a", 4).unwrap();
        assert_eq!(new_events(&page, 4).map(|event| event.seq).collect::<Vec<_>>(), vec![5]);
        assert_eq!(new_events(&page, 5).map(|event| event.seq).collect::<Vec<_>>(), Vec::<u64>::new());
        assert!(validate_page(&page, "job-b", 4).is_err());
        assert!(validate_page(&page, "job-a", 6).is_err());
        let mut stalled = page.clone();
        stalled.events.clear();
        stalled.next_after_seq = 4;
        assert!(validate_page(&stalled, "job-a", 4).is_err());
        let mut wrong = page;
        wrong.events[1].job_id = "job-b".into();
        assert!(validate_page(&wrong, "job-a", 4).is_err());
    }
}
