use anyhow::{Context, Result};
use dashmap::DashMap;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType, SerialNumber,
};
use rustls::sign::CertifiedKey;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CA_PEM_FILENAME: &str = "ca.pem";
const CA_KEY_FILENAME: &str = "ca.key";

pub struct CertManager {
    ca_key: KeyPair,
    ca_cert: rcgen::Certificate,
    ca_cert_der: pki_types::CertificateDer<'static>,
    cache: DashMap<String, Arc<CertifiedKey>>,
    data_dir: PathBuf,
}

impl fmt::Debug for CertManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertManager")
            .field("data_dir", &self.data_dir)
            .field("cached_certs", &self.cache.len())
            .finish()
    }
}

impl CertManager {
    pub fn new_or_load(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let pem_path = data_dir.join(CA_PEM_FILENAME);
        let key_path = data_dir.join(CA_KEY_FILENAME);
        for path in [&pem_path, &key_path] {
            if path.exists() {
                let metadata = std::fs::symlink_metadata(path)?;
                anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "CA credentials must be regular owned files");
            }
        }
        anyhow::ensure!(pem_path.exists() == key_path.exists(), "incomplete CA credential pair");

        if pem_path.exists() && key_path.exists() {
            tracing::info!("loading existing CA from {}", data_dir.display());
            let key_pem = std::fs::read_to_string(&key_path)?;
            let cert_pem = std::fs::read_to_string(&pem_path)?;

            let ca_key = KeyPair::from_pem(&key_pem).context("failed to load CA key")?;

            // Parse the original DER directly from PEM, preserving the original signature.
            // Re-signing with self_signed() would produce a different ECDSA signature each
            // time, changing the cert fingerprint and causing duplicates in trust stores.
            let ca_cert_der = rustls_pemfile::certs(&mut std::io::Cursor::new(cert_pem.as_bytes()))
                .next()
                .ok_or_else(|| anyhow::anyhow!("no certificate found in CA PEM"))?
                .context("failed to parse CA cert DER")?;

            // Reconstruct Certificate object (needed by rcgen's signed_by API for child certs)
            let ca_params = CertificateParams::from_ca_cert_pem(&cert_pem)
                .context("failed to parse CA cert PEM")?;
            let ca_cert = ca_params
                .self_signed(&ca_key)
                .context("failed to reconstruct CA certificate")?;

            Ok(Self {
                ca_key,
                ca_cert,
                ca_cert_der,
                cache: DashMap::new(),
                data_dir: data_dir.to_path_buf(),
            })
        } else {
            tracing::info!("generating new CA certificate");
            let ca_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;

            let mut ca_params = CertificateParams::default();
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            ca_params.key_usages = vec![
                KeyUsagePurpose::DigitalSignature,
                KeyUsagePurpose::KeyCertSign,
                KeyUsagePurpose::CrlSign,
            ];
            ca_params.extended_key_usages = vec![
                ExtendedKeyUsagePurpose::ServerAuth,
                ExtendedKeyUsagePurpose::ClientAuth,
            ];
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "mintcat-proxy CA");
            dn.push(DnType::OrganizationName, "mintcat-proxy");
            dn.push(DnType::CountryName, "CN");
            ca_params.distinguished_name = dn;

            let now = time::OffsetDateTime::now_utc();
            ca_params.not_before = now - time::Duration::days(1);
            ca_params.not_after = now + time::Duration::days(3650);

            let ca_cert = ca_params.self_signed(&ca_key)?;
            let ca_cert_der = pki_types::CertificateDer::from(ca_cert.der().to_vec());

            std::fs::write(&pem_path, ca_cert.pem())?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)] {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            options.open(&key_path)?.write_all(ca_key.serialize_pem().as_bytes())?;

            tracing::info!("CA certificate saved to {}", pem_path.display());

            Ok(Self {
                ca_key,
                ca_cert,
                ca_cert_der,
                cache: DashMap::new(),
                data_dir: data_dir.to_path_buf(),
            })
        }
    }

    pub fn get_or_create(&self, domain: &str) -> Result<Arc<CertifiedKey>> {
        if let Some(entry) = self.cache.get(domain) {
            return Ok(entry.clone());
        }
        let key = Arc::new(self.create_end_cert(domain)?);
        self.cache.insert(domain.to_string(), key.clone());
        Ok(key)
    }

    fn create_end_cert(&self, domain: &str) -> Result<CertifiedKey> {
        let end_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;

        let mut end_params = CertificateParams::default();
        end_params.is_ca = IsCa::NoCa;
        end_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        end_params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];

        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, domain);
        end_params.distinguished_name = dn;

        if let Ok(ip) = domain.parse::<std::net::IpAddr>() {
            end_params.subject_alt_names = vec![SanType::IpAddress(ip)];
        } else {
            end_params.subject_alt_names = vec![SanType::DnsName(domain.try_into()?)];
        }

        let now = time::OffsetDateTime::now_utc();
        end_params.not_before = now - time::Duration::days(1);
        end_params.not_after = now + time::Duration::days(365);
        end_params.serial_number = Some(SerialNumber::from(rand_serial()));

        let end_cert = end_params.signed_by(&end_key, &self.ca_cert, &self.ca_key)?;
        let end_cert_der = pki_types::CertificateDer::from(end_cert.der().to_vec());

        let signing_key =
            rustls::crypto::ring::sign::any_supported_type(&pki_types::PrivateKeyDer::Pkcs8(
                pki_types::PrivatePkcs8KeyDer::from(end_key.serialized_der().to_vec()),
            ))
            .context("failed to create signing key")?;

        Ok(CertifiedKey::new(
            vec![end_cert_der, self.ca_cert_der.clone()],
            signing_key,
        ))
    }

    pub fn ca_pem_path(&self) -> PathBuf {
        self.data_dir.join(CA_PEM_FILENAME)
    }

    pub fn install_ca_trust(&self) -> Result<()> {
        let pem_path = self.ca_pem_path();
        let pem_str = pem_path.to_string_lossy();

        if cfg!(target_os = "macos") {
            let status = std::process::Command::new("/usr/bin/security")
                .args([
                    "add-trusted-cert",
                    "-d",
                    "-r",
                    "trustRoot",
                    "-k",
                    "/Library/Keychains/System.keychain",
                    &pem_str,
                ])
                .status()
                .context("failed to run security command")?;
            if !status.success() {
                anyhow::bail!("security add-trusted-cert failed (exit {})", status);
            }
        } else if cfg!(target_os = "linux") {
            let dest = PathBuf::from("/usr/local/share/ca-certificates/mintcat-proxy.crt");
            std::fs::copy(&pem_path, &dest).context("failed to copy cert. Use sudo.")?;
            let status = std::process::Command::new("/usr/sbin/update-ca-certificates")
                .status()
                .context("failed to run update-ca-certificates")?;
            if !status.success() {
                anyhow::bail!("update-ca-certificates failed (exit {})", status);
            }
        } else if cfg!(target_os = "windows") {
            let status = std::process::Command::new(r"C:\Windows\System32\certutil.exe")
                .args(["-addstore", "-f", "Root", &pem_str])
                .status()
                .context("failed to run certutil")?;
            if !status.success() {
                anyhow::bail!("certutil failed (exit {})", status);
            }
        } else {
            anyhow::bail!("unsupported platform for CA trust installation");
        }

        tracing::info!("CA certificate installed to system trust store");
        Ok(())
    }

    pub fn uninstall_ca_trust(&self) -> Result<()> {
        let status = if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/security")
                .args(["remove-trusted-cert", "-d"]).arg(self.ca_pem_path()).status()?
        } else if cfg!(target_os = "linux") {
            std::fs::remove_file("/usr/local/share/ca-certificates/mintcat-proxy.crt")?;
            std::process::Command::new("/usr/sbin/update-ca-certificates").arg("--fresh").status()?
        } else if cfg!(target_os = "windows") {
            use sha1::Digest;
            let fingerprint = format!("{:x}", sha1::Sha1::digest(self.ca_cert_der.as_ref()));
            std::process::Command::new(r"C:\Windows\System32\certutil.exe")
                .args(["-delstore", "Root", &fingerprint]).status()?
        } else {
            anyhow::bail!("unsupported platform for CA trust removal");
        };
        anyhow::ensure!(status.success(), "system CA removal failed: {status}");
        Ok(())
    }
}

impl rustls::server::ResolvesServerCert for CertManager {
    fn resolve(&self, client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let domain = client_hello.server_name()?;
        tracing::debug!("TLS SNI: {}", domain);
        match self.get_or_create(domain) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::error!("certificate creation failed for {domain}: {error}");
                None
            }
        }
    }
}

fn rand_serial() -> Vec<u8> {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos.to_be_bytes().to_vec()
}
