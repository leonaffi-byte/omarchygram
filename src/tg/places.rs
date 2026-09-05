//! Explicit place-name searches. No background geocoding or location bias.
use super::GeoPoint;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub point: GeoPoint,
}

#[derive(Default)]
pub(super) struct Search {
    state: tokio::sync::Mutex<SearchState>,
}
#[derive(Default)]
struct SearchState {
    last_request: Option<Instant>,
    cache: VecDeque<(String, String, Instant, Vec<Place>)>,
}
impl Search {
    pub async fn run(
        &self,
        http: &reqwest::Client,
        endpoint: &str,
        query: &str,
    ) -> Result<Vec<Place>, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        if query.chars().count() > 256 {
            return Err("Use a shorter place name".into());
        }
        let mut url = reqwest::Url::parse(endpoint)
            .map_err(|_| "Set a valid place search URL in Settings → Privacy")?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err("Place search needs an HTTP or HTTPS URL".into());
        }
        let mut state = self.state.lock().await;
        if let Some((_, _, _, results)) = state.cache.iter().find(|(server, text, at, _)| {
            server == endpoint && text == query && at.elapsed() < Duration::from_secs(3600)
        }) {
            return Ok(results.clone());
        }
        if let Some(last) = state.last_request {
            tokio::time::sleep(Duration::from_secs(1).saturating_sub(last.elapsed())).await;
        }
        state.last_request = Some(Instant::now());
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("limit", "5");
        let mut response = http
            .get(url)
            .send()
            .await
            .map_err(|_| "Place search could not connect. Try again or use coordinates.")?
            .error_for_status()
            .map_err(|_| "Place search is unavailable. Try again or use coordinates.")?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Place search response interrupted")?
        {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err("Place search response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let results = parse(&bytes)?;
        state.cache.push_back((
            endpoint.into(),
            query.into(),
            Instant::now(),
            results.clone(),
        ));
        while state.cache.len() > 32 {
            state.cache.pop_front();
        }
        Ok(results)
    }
}
fn parse(bytes: &[u8]) -> Result<Vec<Place>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid place search response")?;
    let features = value
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or("Invalid place search results")?;
    Ok(features
        .iter()
        .filter_map(|feature| {
            let coords = feature.get("geometry")?.get("coordinates")?.as_array()?;
            let (lon, lat) = (coords.first()?.as_f64()?, coords.get(1)?.as_f64()?);
            if !lat.is_finite()
                || !lon.is_finite()
                || !(-90.0..=90.0).contains(&lat)
                || !(-180.0..=180.0).contains(&lon)
            {
                return None;
            }
            let props = feature.get("properties")?;
            let mut names = Vec::new();
            for key in ["name", "street", "city", "state", "country"] {
                if let Some(name) = props.get(key).and_then(|value| value.as_str())
                    && !name.is_empty()
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
            if names.is_empty() {
                return None;
            }
            Some(Place {
                name: names.join(", ").chars().take(240).collect(),
                point: GeoPoint { lat, lon },
            })
        })
        .take(5)
        .collect())
}
#[cfg(test)]
mod tests {
    #[test]
    fn place_results_validate_coordinates_and_preserve_names() {
        let results = super::parse(br#"{"features":[{"geometry":{"coordinates":[13.4,52.5]},"properties":{"name":"Museum","city":"Berlin"}},{"geometry":{"coordinates":[300,52.5]},"properties":{"name":"Invalid"}}]}"#).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Museum, Berlin");
        assert_eq!(results[0].point.lat, 52.5);
        assert!(super::parse(b"not json").is_err());
    }
}
