//! Segreti delle connessioni nel Portachiavi (`superzed:<id>`, utente `superzed`).
//! Nei test `MemorySecrets`: la piattaforma di test non ha un Portachiavi.
use gpui::{App, Task};
use std::{cell::RefCell, collections::BTreeMap};

pub trait SecretStore: 'static {
    fn read(&self, url: &str, cx: &App) -> Task<anyhow::Result<Option<String>>>;
    fn write(&self, url: &str, secret: &str, cx: &App) -> Task<anyhow::Result<()>>;
    fn delete(&self, url: &str, cx: &App) -> Task<anyhow::Result<()>>;
}

pub struct KeychainSecrets;

impl SecretStore for KeychainSecrets {
    fn read(&self, url: &str, cx: &App) -> Task<anyhow::Result<Option<String>>> {
        let task = cx.read_credentials(url);
        cx.spawn(async move |_| Ok(task.await?.map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())))
    }

    fn write(&self, url: &str, secret: &str, cx: &App) -> Task<anyhow::Result<()>> {
        cx.write_credentials(url, "superzed", secret.as_bytes())
    }

    fn delete(&self, url: &str, cx: &App) -> Task<anyhow::Result<()>> {
        cx.delete_credentials(url)
    }
}

#[derive(Default)]
pub struct MemorySecrets(RefCell<BTreeMap<String, String>>);

impl MemorySecrets {
    pub fn get(&self, url: &str) -> Option<String> {
        self.0.borrow().get(url).cloned()
    }
}

impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.borrow().keys()).finish()
    }
}

impl SecretStore for MemorySecrets {
    fn read(&self, url: &str, _: &App) -> Task<anyhow::Result<Option<String>>> {
        Task::ready(Ok(self.get(url)))
    }

    fn write(&self, url: &str, secret: &str, _: &App) -> Task<anyhow::Result<()>> {
        self.0.borrow_mut().insert(url.to_string(), secret.to_string());
        Task::ready(Ok(()))
    }

    fn delete(&self, url: &str, _: &App) -> Task<anyhow::Result<()>> {
        self.0.borrow_mut().remove(url);
        Task::ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn memory_secrets_read_write_delete(cx: &mut TestAppContext) {
        let s = MemorySecrets::default();
        assert_eq!(cx.update(|cx| s.read("superzed:a", cx)).await.unwrap(), None);
        cx.update(|cx| s.write("superzed:a", "pw", cx)).await.unwrap();
        assert_eq!(cx.update(|cx| s.read("superzed:a", cx)).await.unwrap().as_deref(), Some("pw"));
        cx.update(|cx| s.delete("superzed:a", cx)).await.unwrap();
        assert_eq!(s.get("superzed:a"), None);
        assert!(!format!("{s:?}").contains("pw"));
    }
}
