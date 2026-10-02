use std::{
    fs::File,
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
};

use mintcat_integrator_core::{is_valid_mod_directory, is_valid_zip};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/baseline")
}

fn assert_pak_contains_fixture<R: Read + std::io::Seek>(reader: &mut R) {
    let pak = repak::PakBuilder::new()
        .reader(reader)
        .expect("baseline PAK must be readable");
    let path = "FSD/Content/MintcatFixture/fixture.txt";
    assert!(pak.files().iter().any(|file| file == path));
    assert_eq!(
        pak.get(path, reader)
            .expect("fixture payload must be readable"),
        b"mintcat-r00-fixture\n"
    );
}

#[test]
fn mod_input_fixtures_are_accepted_by_current_runtime() {
    let root = fixture_root().join("mod-inputs");
    let pak_path = root.join("baseline.pak");
    let zip_path = root.join("baseline.zip");
    let directory_path = root.join("directory-basic");

    let mut pak_reader = BufReader::new(File::open(&pak_path).expect("baseline PAK must exist"));
    assert_pak_contains_fixture(&mut pak_reader);

    assert!(is_valid_zip(
        zip_path.to_str().expect("fixture path is UTF-8")
    ));
    let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
    let mut embedded_pak = Vec::new();
    archive
        .by_name("pak/baseline.pak")
        .expect("ZIP must contain its PAK payload")
        .read_to_end(&mut embedded_pak)
        .unwrap();
    assert_pak_contains_fixture(&mut Cursor::new(embedded_pak));

    assert!(is_valid_mod_directory(
        directory_path.to_str().expect("fixture path is UTF-8")
    ));
}
