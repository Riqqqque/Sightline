//! Static names from valorant-api.com: agent and map display names. No key, no player data.
//! Fetched once per run for now; step 2 stores them per game version.

use super::SourceError;
use crate::model::{Role, Uuid};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Default, Clone)]
pub struct Catalog {
    agents: HashMap<Uuid, (String, Role)>,
    /// Keyed by the lower-cased map path, e.g. `/game/maps/ascent/ascent`.
    maps: HashMap<String, String>,
}

impl Catalog {
    pub fn agent(&self, id: &Uuid) -> Option<&str> {
        self.agents.get(id).map(|(name, _)| name.as_str())
    }

    pub fn role(&self, id: &Uuid) -> Role {
        self.agents.get(id).map(|(_, role)| *role).unwrap_or_default()
    }

    pub fn map(&self, url: &str) -> String {
        if let Some(name) = self.maps.get(&url.to_ascii_lowercase()) {
            return name.clone();
        }
        // Unknown map: fall back to the last path segment rather than showing nothing.
        url.rsplit('/').next().unwrap_or("").to_string()
    }

    pub fn add_agent(&mut self, id: &str, name: &str, role: Role) {
        if let Some(id) = Uuid::parse(id) {
            self.agents.insert(id, (name.to_string(), role));
        }
    }

    pub fn add_map(&mut self, url: &str, name: &str) {
        self.maps.insert(url.to_ascii_lowercase(), name.to_string());
    }
}

#[derive(Deserialize)]
struct Listing<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Deserialize)]
struct Agent {
    uuid: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(default)]
    role: Option<RoleRef>,
}

#[derive(Deserialize)]
struct RoleRef {
    #[serde(default, rename = "displayName")]
    display_name: String,
}

#[derive(Deserialize)]
struct Map {
    #[serde(default, rename = "mapUrl")]
    map_url: String,
    #[serde(rename = "displayName")]
    display_name: String,
}

async fn get<T: serde::de::DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<Vec<T>, SourceError> {
    let resp = http.get(url).send().await.map_err(|_| SourceError::Network)?;
    if !resp.status().is_success() {
        return Err(SourceError::Http(resp.status().as_u16()));
    }
    let bytes = resp.bytes().await.map_err(|_| SourceError::Network)?;
    let listing: Listing<T> = serde_json::from_slice(&bytes).map_err(|_| SourceError::Shape("catalog"))?;
    Ok(listing.data)
}

pub async fn fetch(http: &reqwest::Client) -> Result<Catalog, SourceError> {
    let mut catalog = Catalog::default();
    for a in get::<Agent>(http, "https://valorant-api.com/v1/agents?isPlayableCharacter=true").await? {
        let role = a.role.map(|r| Role::parse(&r.display_name)).unwrap_or_default();
        catalog.add_agent(&a.uuid, &a.display_name, role);
    }
    for m in get::<Map>(http, "https://valorant-api.com/v1/maps").await? {
        if !m.map_url.is_empty() {
            catalog.add_map(&m.map_url, &m.display_name);
        }
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        let mut c = Catalog::default();
        c.add_agent("add6443a-41bd-e414-f6ad-e58d267f4e95", "Jett", Role::Duelist);
        c.add_map("/Game/Maps/Ascent/Ascent", "Ascent");
        let jett = Uuid::parse("ADD6443A-41BD-E414-F6AD-E58D267F4E95").unwrap();
        assert_eq!(c.agent(&jett), Some("Jett"));
        assert_eq!(c.role(&jett), Role::Duelist);
        assert_eq!(c.role(&Uuid::parse("00000000-0000-4000-8000-000000000001").unwrap()), Role::Unknown);
        assert_eq!(c.map("/game/maps/ascent/ascent"), "Ascent");
        assert_eq!(c.map("/Game/Maps/NewMap/NewMap"), "NewMap");
    }

    #[test]
    fn listing_shape() {
        let json = r#"{"status":200,"data":[
            {"uuid":"add6443a-41bd-e414-f6ad-e58d267f4e95","displayName":"Jett","role":{"uuid":"x","displayName":"Duelist"}},
            {"uuid":"320b2a48-4d9b-a075-30f1-1f93a9b638fa","displayName":"Sova","role":{"x":1}},
            {"uuid":"569fdd95-4d10-43ab-ca70-79becc718b46","displayName":"Sage","role":null},
            {"uuid":"8e253930-4c05-31dd-1b6c-968525494517","displayName":"Omen"}]}"#;
        let l: Listing<Agent> = serde_json::from_str(json).unwrap();
        assert_eq!(l.data[0].display_name, "Jett");
        let roles: Vec<Role> =
            l.data.into_iter().map(|a| a.role.map(|r| Role::parse(&r.display_name)).unwrap_or_default()).collect();
        assert_eq!(roles, [Role::Duelist, Role::Unknown, Role::Unknown, Role::Unknown]);
    }
}
