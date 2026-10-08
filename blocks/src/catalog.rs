//! Immutable exact state identities supplied by a caller-owned registry report.
//!
//! A catalog is bound to a connection by its owner. It is not a global registry,
//! an old-version block conversion, or evidence of block behavior compatibility.
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_BLOCKS: usize = 8192;
const MAX_STATES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogError(String);

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for CatalogError {}

fn error(message: &str) -> CatalogError {
    CatalogError(message.to_owned())
}

/// Every property is retained even when it does not affect model selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedState {
    id: u32,
    name: String,
    namespace_end: usize,
    properties: BTreeMap<String, String>,
    is_default: bool,
}

impl NamedState {
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn namespace(&self) -> &str {
        &self.name[..self.namespace_end]
    }
    pub fn path(&self) -> &str {
        &self.name[self.namespace_end + 1..]
    }
    pub fn properties(&self) -> &BTreeMap<String, String> {
        &self.properties
    }
    pub fn is_default(&self) -> bool {
        self.is_default
    }
}

/// Dense ID lookup and exact reverse lookup. Missing identities always fail.
#[derive(Debug)]
pub struct StateCatalog {
    states: Vec<NamedState>,
    by_name: HashMap<String, BTreeMap<BTreeMap<String, String>, u32>>,
    defaults: HashMap<String, u32>,
}

impl StateCatalog {
    pub fn from_json(bytes: &[u8]) -> Result<Self, CatalogError> {
        if bytes.len() > MAX_BYTES {
            return Err(error("catalog exceeds input size bound"));
        }
        let raw: UniqueMap<RawBlock> = serde_json::from_slice(bytes)
            .map_err(|e| CatalogError(format!("invalid catalog JSON: {e}")))?;
        if raw.0.is_empty() || raw.0.len() > MAX_BLOCKS {
            return Err(error("catalog block count is outside bounds"));
        }
        let mut indexed = BTreeMap::new();
        let mut by_name = HashMap::new();
        let mut defaults = HashMap::new();
        for (name, block) in raw.0 {
            let namespace_end = validate_name(&name)?;
            if block.states.is_empty() || block.states.len() > MAX_STATES {
                return Err(error("block has invalid state count"));
            }
            let declared = block.properties.unwrap_or_default().0;
            if declared.len() > 64 {
                return Err(error("too many block properties"));
            }
            for (key, values) in &declared {
                validate_property(key)?;
                if values.is_empty() || values.len() > 1024 {
                    return Err(error("invalid declared property values"));
                }
                let mut unique = std::collections::HashSet::new();
                for value in values {
                    validate_property(value)?;
                    if !unique.insert(value) {
                        return Err(error("duplicate declared property value"));
                    }
                }
            }
            let mut reverse = BTreeMap::new();
            for state in block.states {
                if state.id as usize >= MAX_STATES || indexed.len() >= MAX_STATES {
                    return Err(error("catalog state count or ID exceeds bound"));
                }
                let properties = state.properties.unwrap_or_default().0;
                if properties.len() != declared.len() {
                    return Err(error("state property keys differ from block definition"));
                }
                for (key, value) in &properties {
                    validate_property(key)?;
                    validate_property(value)?;
                    if !declared
                        .get(key)
                        .is_some_and(|values| values.contains(value))
                    {
                        return Err(error("state property is not declared by its block"));
                    }
                }
                if reverse.insert(properties.clone(), state.id).is_some() {
                    return Err(error("duplicate named state"));
                }
                if state.default && defaults.insert(name.clone(), state.id).is_some() {
                    return Err(error("block has multiple default states"));
                }
                let named = NamedState {
                    id: state.id,
                    name: name.clone(),
                    namespace_end,
                    properties,
                    is_default: state.default,
                };
                if indexed.insert(state.id, named).is_some() {
                    return Err(error("duplicate numeric state ID"));
                }
            }
            if !defaults.contains_key(&name) {
                return Err(error("block has no declared default state"));
            }
            by_name.insert(name, reverse);
        }
        let mut states = Vec::with_capacity(indexed.len());
        for (expected, (id, state)) in indexed.into_iter().enumerate() {
            if id as usize != expected {
                return Err(error("catalog state IDs must be dense from zero"));
            }
            states.push(state);
        }
        Ok(Self {
            states,
            by_name,
            defaults,
        })
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
    pub fn state_count(&self) -> u32 {
        self.states.len() as u32
    }
    pub fn states(&self) -> &[NamedState] {
        &self.states
    }
    pub fn state(&self, id: u32) -> Result<&NamedState, CatalogError> {
        self.states
            .get(id as usize)
            .ok_or_else(|| error("unknown numeric state ID"))
    }
    pub fn find_state(
        &self,
        name: &str,
        properties: &BTreeMap<String, String>,
    ) -> Result<&NamedState, CatalogError> {
        let id = self
            .by_name
            .get(name)
            .and_then(|states| states.get(properties))
            .ok_or_else(|| error("unknown exact named state"))?;
        self.state(*id)
    }
    pub fn default_state(&self, name: &str) -> Result<&NamedState, CatalogError> {
        let id = self
            .defaults
            .get(name)
            .ok_or_else(|| error("unknown block name"))?;
        self.state(*id)
    }
}

fn validate_name(name: &str) -> Result<usize, CatalogError> {
    let (namespace, path) = name
        .split_once(':')
        .ok_or_else(|| error("block name requires a namespace"))?;
    let valid_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b);
    if namespace.is_empty()
        || path.is_empty()
        || name.len() > 512
        || !namespace.bytes().all(valid_char)
        || !path.bytes().all(|b| valid_char(b) || b == b'/')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(error("invalid block resource name"));
    }
    Ok(namespace.len())
}

fn validate_property(value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    {
        return Err(error("invalid property name or value"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct RawBlock {
    properties: Option<UniqueMap<Vec<String>>>,
    states: Vec<RawState>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawState {
    id: u32,
    properties: Option<UniqueMap<String>>,
    #[serde(default)]
    default: bool,
}

/// Standard maps silently overwrite duplicate JSON keys; registries must not.
struct UniqueMap<T>(BTreeMap<String, T>);
impl<T> Default for UniqueMap<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for UniqueMap<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for UniqueVisitor<T> {
            type Value = UniqueMap<T>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with unique keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut result = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, T>()? {
                    if result.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate catalog object key"));
                    }
                }
                Ok(UniqueMap(result))
            }
        }
        deserializer.deserialize_map(UniqueVisitor(std::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SYNTHETIC: &str = r#"{"fixture:air":{"states":[{"id":0,"default":true}]},"fixture:axis":{"properties":{"axis":["x","y"]},"states":[{"id":1,"properties":{"axis":"x"}},{"id":2,"properties":{"axis":"y"},"default":true}]}}"#;

    #[test]
    fn exact_round_trip_preserves_all_identities() {
        let catalog = StateCatalog::from_json(SYNTHETIC.as_bytes()).unwrap();
        assert_eq!(catalog.state_count(), 3);
        for state in catalog.states() {
            assert_eq!(
                catalog
                    .find_state(state.name(), state.properties())
                    .unwrap(),
                state
            );
        }
        assert_eq!(catalog.state(1).unwrap().namespace(), "fixture");
        assert_eq!(catalog.state(1).unwrap().path(), "axis");
        assert_eq!(catalog.default_state("fixture:axis").unwrap().id(), 2);
        assert!(catalog.state(3).is_err());
        assert!(catalog
            .find_state("fixture:axis", &BTreeMap::new())
            .is_err());
        assert!(catalog.default_state("missing:block").is_err());
    }

    #[test]
    fn malformed_identity_tables_are_rejected() {
        for source in [
            SYNTHETIC.replace("\"id\":2", "\"id\":3"),
            SYNTHETIC.replace("\"id\":2", "\"id\":1"),
            SYNTHETIC.replace("\"axis\":\"y\"", "\"axis\":\"x\""),
            SYNTHETIC.replace("\"axis\":\"y\"", "\"axis\":\"z\""),
            SYNTHETIC.replace("\"id\":1", "\"id\":1,\"default\":true"),
            SYNTHETIC.replace(",\"default\":true", ""),
            SYNTHETIC.replace("fixture:air", "fixture:../air"),
            SYNTHETIC.replace("\"axis\":\"y\"", "\"axis\":\"y\",\"axis\":\"x\""),
            r#"{"a:b":{"states":[{"id":0,"default":true}]},"a:b":{"states":[{"id":1,"default":true}]}}"#.to_owned(),
        ] { assert!(StateCatalog::from_json(source.as_bytes()).is_err(), "{}", source); }
    }

    #[test]
    #[ignore = "requires caller-owned generated report in LEAFISH_BLOCK_CATALOG"]
    fn installed_catalog_exact_round_trip() {
        let path = std::env::var_os("LEAFISH_BLOCK_CATALOG")
            .expect("set LEAFISH_BLOCK_CATALOG to a locally generated blocks.json");
        let bytes = std::fs::read(path).unwrap();
        let catalog = StateCatalog::from_json(&bytes).unwrap();
        assert_eq!(catalog.state_count(), 26684);
        for state in catalog.states() {
            assert_eq!(
                catalog
                    .find_state(state.name(), state.properties())
                    .unwrap()
                    .id(),
                state.id()
            );
        }
    }
}
