use anyhow::{Context, Result};
use oo7::{Keyring, Secret};
use tokio::runtime::Builder;

const APP: &str = "smart-clock";

fn attributes(name: &str) -> [(&'static str, &str); 2] {
    [("app", APP), ("service", name)]
}

pub async fn get(name: &str) -> Result<Option<String>> {
    let keyring = Keyring::new().await.context("opening the system keyring")?;
    let items = keyring
        .search_items(&attributes(name))
        .await
        .context("searching the system keyring")?;
    let Some(item) = items.into_iter().next() else {
        return Ok(None);
    };
    let secret = item
        .secret()
        .await
        .context("reading a secret from the system keyring")?;
    secret
        .as_str()
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("system keyring secret is not valid UTF-8"))
}

pub async fn set(name: &str, value: Option<&str>) -> Result<()> {
    let keyring = Keyring::new().await.context("opening the system keyring")?;
    let attributes = attributes(name);
    match value.filter(|value| !value.is_empty()) {
        Some(value) => keyring
            .create_item(
                &format!("Smart Clock · {name}"),
                &attributes,
                Secret::text(value),
                true,
            )
            .await
            .context("saving a secret to the system keyring"),
        None => keyring
            .delete(&attributes)
            .await
            .context("removing a secret from the system keyring"),
    }
}

pub fn get_blocking(name: &str) -> Result<Option<String>> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building keyring runtime")?;
    runtime.block_on(get(name))
}

pub fn set_blocking(name: &str, value: Option<&str>) -> Result<()> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building keyring runtime")?;
    runtime.block_on(set(name, value))
}
