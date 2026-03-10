use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;

use crate::{
    settings,
    types::{ProviderKind, SecretBackend, SecretRef},
};

const SECRET_NAMESPACE_PREFIX: &str = "agent-llm";

pub trait SecretStore {
    fn backend(&self) -> SecretBackend;
    fn store_auth_profile_secret(
        &self,
        provider: ProviderKind,
        profile_name: &str,
        secret_value: &str,
    ) -> Result<SecretRef>;
    fn read_secret(&self, secret_ref: &SecretRef) -> Result<String>;
    fn delete_secret(&self, secret_ref: &SecretRef) -> Result<()>;
}

#[derive(Clone, Debug)]
pub enum LocalSecretStore {
    #[cfg(target_os = "macos")]
    Keychain(KeychainSecretStore),
    File(FileSecretStore),
}

impl LocalSecretStore {
    pub fn detect() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            return Ok(Self::Keychain(KeychainSecretStore::default()));
        }

        #[allow(unreachable_code)]
        Ok(Self::File(FileSecretStore::default()?))
    }

    pub fn file_backed(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::File(FileSecretStore::new(
            path.as_ref().to_path_buf(),
        )?))
    }
}

impl SecretStore for LocalSecretStore {
    fn backend(&self) -> SecretBackend {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain(store) => store.backend(),
            Self::File(store) => store.backend(),
        }
    }

    fn store_auth_profile_secret(
        &self,
        provider: ProviderKind,
        profile_name: &str,
        secret_value: &str,
    ) -> Result<SecretRef> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain(store) => {
                store.store_auth_profile_secret(provider, profile_name, secret_value)
            }
            Self::File(store) => {
                store.store_auth_profile_secret(provider, profile_name, secret_value)
            }
        }
    }

    fn read_secret(&self, secret_ref: &SecretRef) -> Result<String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain(store) if secret_ref.backend == SecretBackend::Keychain => {
                store.read_secret(secret_ref)
            }
            Self::File(store) if secret_ref.backend == SecretBackend::File => {
                store.read_secret(secret_ref)
            }
            #[cfg(target_os = "macos")]
            Self::Keychain(_) => Err(anyhow!(
                "secret ref backend `{}` is not supported by the active keychain store",
                secret_ref.backend.as_str()
            )),
            Self::File(_) => Err(anyhow!(
                "secret ref backend `{}` is not supported by the active file store",
                secret_ref.backend.as_str()
            )),
        }
    }

    fn delete_secret(&self, secret_ref: &SecretRef) -> Result<()> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain(store) if secret_ref.backend == SecretBackend::Keychain => {
                store.delete_secret(secret_ref)
            }
            Self::File(store) if secret_ref.backend == SecretBackend::File => {
                store.delete_secret(secret_ref)
            }
            #[cfg(target_os = "macos")]
            Self::Keychain(_) => Ok(()),
            Self::File(_) => Ok(()),
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, Default)]
pub struct KeychainSecretStore;

#[cfg(target_os = "macos")]
impl SecretStore for KeychainSecretStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::Keychain
    }

    fn store_auth_profile_secret(
        &self,
        provider: ProviderKind,
        profile_name: &str,
        secret_value: &str,
    ) -> Result<SecretRef> {
        use security_framework::passwords::set_generic_password;

        let account = keychain_account(provider, profile_name);
        set_generic_password(SECRET_NAMESPACE_PREFIX, &account, secret_value.as_bytes())
            .with_context(|| format!("failed to store keychain secret for `{account}`"))?;
        Ok(SecretRef::new(SecretBackend::Keychain, account))
    }

    fn read_secret(&self, secret_ref: &SecretRef) -> Result<String> {
        use security_framework::passwords::get_generic_password;

        let bytes = get_generic_password(SECRET_NAMESPACE_PREFIX, &secret_ref.key)
            .with_context(|| format!("failed to read keychain secret `{}`", secret_ref.key))?;
        String::from_utf8(bytes).context("keychain secret was not valid UTF-8")
    }

    fn delete_secret(&self, secret_ref: &SecretRef) -> Result<()> {
        use security_framework::passwords::delete_generic_password;

        delete_generic_password(SECRET_NAMESPACE_PREFIX, &secret_ref.key)
            .with_context(|| format!("failed to delete keychain secret `{}`", secret_ref.key))?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    pub fn default() -> Result<Self> {
        let root = settings::default_data_dir()?.join("secrets");
        Self::new(root)
    }

    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self { root })
    }

    fn secret_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.secret"))
    }
}

impl SecretStore for FileSecretStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::File
    }

    fn store_auth_profile_secret(
        &self,
        provider: ProviderKind,
        profile_name: &str,
        secret_value: &str,
    ) -> Result<SecretRef> {
        let key = format!(
            "{}-{}-{}",
            provider.as_str(),
            sanitize(profile_name),
            random_suffix()
        );
        let path = self.secret_path(&key);
        fs::write(&path, secret_value)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(SecretRef::new(SecretBackend::File, key))
    }

    fn read_secret(&self, secret_ref: &SecretRef) -> Result<String> {
        let path = self.secret_path(&secret_ref.key);
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    fn delete_secret(&self, secret_ref: &SecretRef) -> Result<()> {
        let path = self.secret_path(&secret_ref.key);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to delete {}", path.display()))
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn keychain_account(provider: ProviderKind, profile_name: &str) -> String {
    format!(
        "{SECRET_NAMESPACE_PREFIX}/profiles/{}/{}",
        provider.as_str(),
        sanitize(profile_name)
    )
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn random_suffix() -> String {
    let mut bytes = [0u8; 6];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_secret_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is valid")
            .as_nanos();
        std::env::temp_dir().join(format!("agent-llm-secrets-{nonce}"))
    }

    #[test]
    fn file_store_round_trips_secret_material() {
        let store = FileSecretStore::new(test_secret_dir()).expect("store creates");
        let secret_ref = store
            .store_auth_profile_secret(ProviderKind::OpenAi, "codex-session", "super-secret")
            .expect("secret stored");
        assert_eq!(secret_ref.backend, SecretBackend::File);

        let value = store.read_secret(&secret_ref).expect("secret read");
        assert_eq!(value, "super-secret");

        store.delete_secret(&secret_ref).expect("secret deleted");
        let error = store
            .read_secret(&secret_ref)
            .expect_err("deleted secret should not be readable");
        assert!(error.to_string().contains("failed to read"));
    }
}
