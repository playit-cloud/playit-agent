use serde::de::DeserializeOwned;

pub async fn load_config<T: DeserializeOwned>(path: &str) -> Option<T> {
    let data = tokio::fs::read_to_string(path).await.ok()?;

    if path.ends_with(".json") {
        return serde_json::from_str(&data).ok();
    }

    if path.ends_with(".toml") {
        return Some(toml::from_str(&data).unwrap());
    }

    if path.ends_with(".yaml") || path.ends_with(".yml") {
        return serde_yaml::from_str(&data).ok();
    }

    None
}