//! Resource-only model resolution and state selection, independent of OpenGL.
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub type ModelResult<T> = Result<T, String>;
const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 4096;

#[derive(Clone, Debug)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Match(String, Vec<String>),
}

impl Condition {
    pub fn parse(value: &Value) -> ModelResult<Self> {
        Self::parse_at(value, 0)
    }

    fn parse_at(value: &Value, depth: usize) -> ModelResult<Self> {
        if depth >= MAX_DEPTH {
            return Err("multipart condition nesting exceeds bound".into());
        }
        let object = value
            .as_object()
            .ok_or("multipart condition must be an object")?;
        if object.len() > 64 {
            return Err("too many multipart condition keys".into());
        }
        let mut conditions = Vec::new();
        for (key, value) in object {
            if key == "AND" || key == "OR" {
                let values = value.as_array().ok_or("AND/OR requires an array")?;
                if values.is_empty() || values.len() > 256 {
                    return Err("invalid condition array length".into());
                }
                let children = values
                    .iter()
                    .map(|value| Self::parse_at(value, depth + 1))
                    .collect::<ModelResult<Vec<_>>>()?;
                conditions.push(if key == "AND" {
                    Self::All(children)
                } else {
                    Self::Any(children)
                });
            } else {
                let token = match value {
                    Value::String(value) => value.clone(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) if value.is_i64() || value.is_u64() => value.to_string(),
                    _ => {
                        return Err("multipart property must be a string, boolean or integer".into())
                    }
                };
                let alternatives = token.split('|').map(str::to_owned).collect::<Vec<_>>();
                if key.is_empty()
                    || key.len() > 128
                    || alternatives.len() > 1024
                    || alternatives
                        .iter()
                        .any(|value| value.is_empty() || value.len() > 128)
                {
                    return Err("invalid multipart property alternatives".into());
                }
                conditions.push(Self::Match(key.clone(), alternatives));
            }
        }
        Ok(Self::All(conditions))
    }

    pub fn matches(&self, properties: &BTreeMap<String, String>) -> bool {
        self.matches_with(&|key, value| properties.get(key).is_some_and(|actual| actual == value))
    }

    pub fn matches_with<F: Fn(&str, &str) -> bool>(&self, matches: &F) -> bool {
        match self {
            Self::All(children) => children.iter().all(|child| child.matches_with(matches)),
            Self::Any(children) => children.iter().any(|child| child.matches_with(matches)),
            Self::Match(key, alternatives) => alternatives.iter().any(|value| matches(key, value)),
        }
    }
}

/// Variant selectors constrain a subset of the state's properties.
pub fn variant_matches(selector: &str, properties: &BTreeMap<String, String>) -> ModelResult<bool> {
    if selector.is_empty() {
        return Ok(true);
    }
    if selector.len() > 8192 {
        return Err("variant selector exceeds bound".into());
    }
    let mut keys = HashSet::new();
    let mut matched = true;
    for clause in selector.split(',') {
        let (key, value) = clause
            .split_once('=')
            .ok_or("invalid variant property selector")?;
        if key.is_empty()
            || value.is_empty()
            || key.len() > 128
            || value.len() > 128
            || value.contains('=')
            || !keys.insert(key)
            || keys.len() > 64
        {
            return Err("invalid or duplicate variant property".into());
        }
        matched &= properties.get(key).is_some_and(|actual| actual == value);
    }
    Ok(matched)
}

/// Resolve a resource location without losing an explicit namespace.
pub fn resource_location<'a>(
    value: &'a str,
    default_namespace: &'a str,
) -> ModelResult<(&'a str, &'a str)> {
    let (namespace, path) = value.split_once(':').unwrap_or((default_namespace, value));
    let valid =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte);
    if value.len() > 512
        || namespace.is_empty()
        || path.is_empty()
        || !namespace.bytes().all(valid)
        || !path.bytes().all(|byte| valid(byte) || byte == b'/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("invalid asset resource location".into());
    }
    Ok((namespace, path))
}

/// `load(namespace, path)` reads a model JSON using a resource-pack stack.
/// The path is the model identifier, excluding `models/` and `.json`.
pub fn resolve_model<F>(namespace: &str, path: &str, load: &F) -> ModelResult<Value>
where
    F: Fn(&str, &str) -> ModelResult<Value>,
{
    fn visit<F>(
        namespace: &str,
        path: &str,
        load: &F,
        chain: &mut Vec<String>,
    ) -> ModelResult<Value>
    where
        F: Fn(&str, &str) -> ModelResult<Value>,
    {
        resource_location(&format!("{namespace}:{path}"), "minecraft")?;
        let key = format!("{namespace}:{path}");
        if chain.len() >= MAX_DEPTH || chain.contains(&key) {
            return Err("cyclic or too-deep model parent chain".into());
        }
        chain.push(key);
        let source = load(namespace, path)?;
        let source = source.as_object().ok_or("model must be an object")?;
        if source.contains_key("loader") {
            return Err("custom model loader requires an explicit adapter".into());
        }
        let mut resolved = if let Some(parent) = source.get("parent") {
            let parent = parent.as_str().ok_or("model parent must be a string")?;
            let (parent_namespace, parent_path) = resource_location(parent, "minecraft")?;
            if parent_namespace == "minecraft" && parent_path.starts_with("builtin/") {
                let mut object = Map::new();
                object.insert("parent".into(), Value::String(parent_path.to_owned()));
                object
            } else {
                visit(parent_namespace, parent_path, load, chain)?
                    .as_object()
                    .unwrap()
                    .clone()
            }
        } else {
            Map::new()
        };
        for (key, value) in source {
            if key == "parent" {
                continue;
            }
            if key == "textures" {
                let inherited = resolved
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                let inherited = inherited
                    .as_object_mut()
                    .ok_or("model textures must be an object")?;
                for (name, texture) in value
                    .as_object()
                    .ok_or("model textures must be an object")?
                {
                    if name.is_empty() || name.len() > 128 || !texture.is_string() {
                        return Err("invalid model texture variable".into());
                    }
                    inherited.insert(name.clone(), texture.clone());
                }
            } else {
                // In particular, a child's elements replace its parent's array.
                resolved.insert(key.clone(), value.clone());
            }
        }
        chain.pop();
        Ok(Value::Object(resolved))
    }
    let resolved = visit(namespace, path, load, &mut Vec::new())?;
    validate_model(&resolved)?;
    Ok(resolved)
}

pub fn resolve_texture(variables: &Map<String, Value>, value: &str) -> ModelResult<String> {
    let mut texture = value;
    let mut seen = HashSet::new();
    // BlockModel.getMaterial also accepts a map key without '#'. Only the
    // face's initial name uses this shorthand; texture-map values are materials
    // unless they explicitly start with '#'.
    if !value.starts_with('#') {
        if let Some(mapped) = variables.get(value) {
            seen.insert(value);
            texture = mapped.as_str().ok_or("invalid texture alias value")?;
        }
    }
    while let Some(name) = texture.strip_prefix('#') {
        if seen.len() >= MAX_DEPTH || !seen.insert(name) {
            return Err("cyclic or too-deep texture aliases".into());
        }
        texture = variables
            .get(name)
            .and_then(Value::as_str)
            .ok_or("missing texture alias")?;
    }
    let (namespace, path) = resource_location(texture, "minecraft")?;
    Ok(format!("{namespace}:{path}"))
}

fn numbers(value: Option<&Value>, length: usize) -> ModelResult<()> {
    let values = value
        .and_then(Value::as_array)
        .ok_or("missing numeric model array")?;
    if values.len() != length
        || values.iter().any(|v| {
            !v.as_f64()
                .is_some_and(|v| v.is_finite() && v.abs() <= 1_048_576.0)
        })
    {
        return Err("invalid bounded numeric model array".into());
    }
    Ok(())
}

/// Check all fields consumed by the legacy cuboid mesh builder before access.
pub fn validate_model(model: &Value) -> ModelResult<()> {
    let model = model.as_object().ok_or("model must be an object")?;
    let empty = Map::new();
    let textures = model
        .get("textures")
        .map(|v| v.as_object().ok_or("invalid texture map"))
        .transpose()?
        .unwrap_or(&empty);
    if textures.len() > 4096 {
        return Err("texture variable count exceeds bound".into());
    }
    if let Some(ao) = model.get("ambientocclusion") {
        if !ao.is_boolean() {
            return Err("invalid ambient occlusion flag".into());
        }
    }
    if let Some(elements) = model.get("elements") {
        let elements = elements.as_array().ok_or("elements must be an array")?;
        if elements.len() > MAX_ELEMENTS {
            return Err("model element count exceeds bound".into());
        }
        for element in elements {
            let element = element.as_object().ok_or("element must be an object")?;
            numbers(element.get("from"), 3)?;
            numbers(element.get("to"), 3)?;
            if let Some(shade) = element.get("shade") {
                if !shade.is_boolean() {
                    return Err("invalid shade flag".into());
                }
            }
            if let Some(rotation) = element.get("rotation") {
                let rotation = rotation.as_object().ok_or("invalid element rotation")?;
                numbers(rotation.get("origin"), 3)?;
                if !matches!(
                    rotation.get("axis").and_then(Value::as_str),
                    Some("x" | "y" | "z")
                ) || !rotation
                    .get("angle")
                    .and_then(Value::as_f64)
                    .is_some_and(|a| [-45.0, -22.5, 0.0, 22.5, 45.0].contains(&a))
                {
                    return Err("invalid element rotation axis or angle".into());
                }
                if let Some(flag) = rotation.get("rescale") {
                    if !flag.is_boolean() {
                        return Err("invalid rescale flag".into());
                    }
                }
            }
            let faces = element
                .get("faces")
                .and_then(Value::as_object)
                .ok_or("missing element faces")?;
            for (direction, face) in faces {
                if !["up", "down", "north", "south", "east", "west"].contains(&direction.as_str()) {
                    return Err("invalid face direction".into());
                }
                let face = face.as_object().ok_or("invalid face object")?;
                if let Some(uv) = face.get("uv") {
                    numbers(Some(uv), 4)?;
                }
                resolve_texture(
                    textures,
                    face.get("texture")
                        .and_then(Value::as_str)
                        .ok_or("missing face texture")?,
                )?;
                if let Some(rotation) = face.get("rotation") {
                    if !rotation
                        .as_u64()
                        .is_some_and(|v| [0, 90, 180, 270].contains(&v))
                    {
                        return Err("invalid face rotation".into());
                    }
                }
                if let Some(cull) = face.get("cullface") {
                    // Direction.byName maps unknown strings to no culling.
                    // Vanilla scaffolding contains "bottom".
                    if !cull.as_str().is_some_and(|v| v.len() <= 128) {
                        return Err("invalid cull face".into());
                    }
                }
                if let Some(tint) = face.get("tintindex") {
                    if !tint.as_i64().is_some_and(|v| (-1..=255).contains(&v)) {
                        return Err("invalid tint index".into());
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn application_weight(value: &Value) -> ModelResult<u32> {
    match value.get("weight") {
        None => Ok(1),
        Some(weight) => weight
            .as_u64()
            .filter(|w| (1..=u32::MAX as u64).contains(w))
            .map(|w| w as u32)
            .ok_or_else(|| "invalid positive model weight".into()),
    }
}

/// Return the active application groups; an empty multipart result is valid.
pub fn select_applications<'a>(
    definition: &'a Value,
    properties: &BTreeMap<String, String>,
) -> ModelResult<Vec<&'a Value>> {
    let object = definition
        .as_object()
        .ok_or("blockstate definition must be an object")?;
    match (object.get("variants"), object.get("multipart")) {
        (Some(variants), None) => {
            let variants = variants.as_object().ok_or("variants must be an object")?;
            if variants.len() > 65536 {
                return Err("variant count exceeds bound".into());
            }
            let mut selected = Vec::new();
            for (selector, application) in variants {
                if variant_matches(selector, properties)? {
                    selected.push(application);
                }
            }
            if selected.len() != 1 {
                return Err("named state must select exactly one variant".into());
            }
            Ok(selected)
        }
        (None, Some(parts)) => {
            let parts = parts.as_array().ok_or("multipart must be an array")?;
            if parts.len() > 4096 {
                return Err("multipart count exceeds bound".into());
            }
            let mut selected = Vec::new();
            for part in parts {
                let condition = part.get("when").map(Condition::parse).transpose()?;
                let application = part.get("apply").ok_or("multipart is missing apply")?;
                if condition
                    .as_ref()
                    .is_none_or(|rule| rule.matches(properties))
                {
                    selected.push(application);
                }
            }
            Ok(selected)
        }
        _ => Err("blockstate requires exactly one of variants or multipart".into()),
    }
}

#[derive(Debug, serde::Serialize)]
pub struct CatalogModelAudit {
    pub blocks_checked: usize,
    pub states_checked: usize,
    pub models_checked: usize,
    pub static_models: usize,
    pub empty_models: Vec<String>,
    pub zero_application_states: usize,
    pub faces_checked: usize,
    pub texture_references: Vec<String>,
}

/// Exhaustive headless audit using the same selectors and resolver as Factory.
/// `read(namespace, path)` uses paths such as `blockstates/stone.json`.
/// Empty models are reported separately; they are never classified as air.
pub fn audit_catalog<F>(
    catalog: &leafish_blocks::catalog::StateCatalog,
    read: &F,
) -> ModelResult<CatalogModelAudit>
where
    F: Fn(&str, &str) -> ModelResult<Value>,
{
    let mut blockstates = HashMap::new();
    let mut references = HashSet::new();
    let mut zero_application_states = 0;
    for state in catalog.states() {
        if !blockstates.contains_key(state.name()) {
            let definition = read(
                state.namespace(),
                &format!("blockstates/{}.json", state.path()),
            )?;
            blockstates.insert(state.name().to_owned(), definition);
        }
        let applications = select_applications(&blockstates[state.name()], state.properties())
            .map_err(|message| format!("{} state {}: {}", state.name(), state.id(), message))?;
        if applications.is_empty() {
            zero_application_states += 1;
        }
        for application in applications {
            let choices = application
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(std::slice::from_ref(application));
            if choices.is_empty() || choices.len() > 4096 {
                return Err("invalid model application list".into());
            }
            for choice in choices {
                let name = choice
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or("missing application model")?;
                let (namespace, path) = resource_location(name, "minecraft")?;
                application_weight(choice)?;
                for axis in ["x", "y"] {
                    if choice.get(axis).is_some_and(|value| {
                        !value
                            .as_u64()
                            .is_some_and(|v| [0, 90, 180, 270].contains(&v))
                    }) {
                        return Err("invalid model application rotation".into());
                    }
                }
                references.insert((namespace.to_owned(), path.to_owned()));
            }
        }
    }
    let mut audit = CatalogModelAudit {
        blocks_checked: blockstates.len(),
        states_checked: catalog.len(),
        models_checked: references.len(),
        static_models: 0,
        empty_models: vec![],
        zero_application_states,
        faces_checked: 0,
        texture_references: vec![],
    };
    let mut texture_references = HashSet::new();
    for (namespace, path) in references {
        let model = resolve_model(&namespace, &path, &|namespace, path| {
            read(namespace, &format!("models/{path}.json"))
        })
        .map_err(|message| format!("{namespace}:{path}: {message}"))?;
        let elements = model.get("elements").and_then(Value::as_array);
        if elements.is_none_or(Vec::is_empty) {
            audit.empty_models.push(format!("{namespace}:{path}"));
        } else {
            audit.static_models += 1;
            audit.faces_checked += elements
                .unwrap()
                .iter()
                .map(|element| element["faces"].as_object().unwrap().len())
                .sum::<usize>();
            let empty = Map::new();
            let variables = model
                .get("textures")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            for element in elements.unwrap() {
                for face in element["faces"].as_object().unwrap().values() {
                    texture_references.insert(resolve_texture(
                        variables,
                        face["texture"].as_str().unwrap(),
                    )?);
                }
            }
        }
    }
    audit.empty_models.sort();
    audit.texture_references = texture_references.into_iter().collect();
    audit.texture_references.sort();
    Ok(audit)
}

/// Confirm every resolved face texture exists in the active resource stack.
pub fn verify_texture_references<F>(audit: &CatalogModelAudit, exists: &F) -> ModelResult<()>
where
    F: Fn(&str, &str) -> bool,
{
    for texture in &audit.texture_references {
        let (namespace, path) = resource_location(texture, "minecraft")?;
        if !exists(namespace, &format!("textures/{path}.png")) {
            return Err(format!("missing resolved face texture {texture}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_conditions_match_exact_alternative_tokens() {
        let rule =
            Condition::parse(&json!({"AND":[{"power":"1|10"},{"OR":[{"axis":"x"},{"axis":"z"}]}]}))
                .unwrap();
        let mut props = BTreeMap::from([("power".into(), "1".into()), ("axis".into(), "x".into())]);
        assert!(rule.matches(&props));
        props.insert("power".into(), "0".into());
        assert!(!rule.matches(&props));
        props.insert("power".into(), "11".into());
        assert!(!rule.matches(&props));
        assert!(Condition::parse(&json!({"AND":1})).is_err());
    }

    #[test]
    fn variant_selection_uses_property_subset_and_rejects_ambiguity() {
        let props = BTreeMap::from([
            ("axis".into(), "x".into()),
            ("waterlogged".into(), "true".into()),
        ]);
        assert!(variant_matches("axis=x", &props).unwrap());
        assert!(variant_matches("axis=x,axis=y", &props).is_err());
        assert!(select_applications(
            &json!({"variants":{"":{"model":"a:b"},"axis=x":{"model":"a:c"}}}),
            &props
        )
        .is_err());
        assert!(select_applications(
            &json!({"multipart":[{"when":{"axis":"z"},"apply":{"model":"a:b"}}]}),
            &props
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn parent_namespace_elements_override_and_texture_resolution() {
        let models = BTreeMap::from([
            (
                ("fixture".to_owned(), "child".to_owned()),
                json!({"parent":"other:parent","textures":{"alias":"#base","base":"other:block/exact"},"elements":[{"from":[1,1,1],"to":[2,2,2],"faces":{"north":{"texture":"#alias"},"south":{"texture":"other:block/direct"}}}]}),
            ),
            (
                ("other".to_owned(), "parent".to_owned()),
                json!({"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{}}]}),
            ),
        ]);
        let resolved = resolve_model("fixture", "child", &|n, p| {
            models
                .get(&(n.into(), p.into()))
                .cloned()
                .ok_or("missing model".into())
        })
        .unwrap();
        assert_eq!(resolved["elements"].as_array().unwrap().len(), 1);
        assert_eq!(resolved["elements"][0]["from"][0], 1);
        assert_eq!(
            resolve_texture(resolved["textures"].as_object().unwrap(), "#alias").unwrap(),
            "other:block/exact"
        );
        assert_eq!(
            resolve_texture(&Map::new(), "other:block/direct").unwrap(),
            "other:block/direct"
        );
        assert_eq!(
            resolve_texture(resolved["textures"].as_object().unwrap(), "alias").unwrap(),
            "other:block/exact"
        );
    }

    #[test]
    fn cycles_invalid_geometry_and_custom_loaders_fail_explicitly() {
        assert!(resolve_model("a", "b", &|_, _| Ok(json!({"parent":"a:b"}))).is_err());
        assert!(resolve_model("a", "b", &|_, _| Ok(json!({"loader":"custom:dynamic"}))).is_err());
        assert!(resolve_texture(json!({"a":"#b","b":"#a"}).as_object().unwrap(), "#a").is_err());
        assert!(
            validate_model(&json!({"elements":[{"from":[0],"to":[1,1,1],"faces":{}}]})).is_err()
        );
        assert!(application_weight(&json!({"weight":0})).is_err());
    }

    #[test]
    #[ignore = "requires caller-owned LEAFISH_BLOCK_CATALOG and LEAFISH_CLIENT_JAR"]
    fn installed_catalog_models_resolve_without_graphics() {
        let report = std::env::var_os("LEAFISH_BLOCK_CATALOG")
            .expect("set LEAFISH_BLOCK_CATALOG to generated blocks.json");
        let archive = std::env::var_os("LEAFISH_CLIENT_JAR")
            .expect("set LEAFISH_CLIENT_JAR to a locally installed client archive");
        let catalog =
            leafish_blocks::catalog::StateCatalog::from_json(&std::fs::read(report).unwrap())
                .unwrap();
        let zip = std::cell::RefCell::new(
            zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap(),
        );
        let audit = audit_catalog(&catalog, &|namespace, path| {
            let mut zip = zip.borrow_mut();
            let file = zip
                .by_name(&format!("assets/{namespace}/{path}"))
                .map_err(|error| error.to_string())?;
            super::super::read_asset_json(file)
        })
        .unwrap();
        assert_eq!(audit.states_checked, 26684);
        assert_eq!(audit.blocks_checked, 1060);
        verify_texture_references(&audit, &|namespace, path| {
            zip.borrow_mut()
                .by_name(&format!("assets/{namespace}/{path}"))
                .is_ok()
        })
        .unwrap();
        println!(
            "{}",
            serde_json::json!({"blocks":audit.blocks_checked,"states":audit.states_checked,
            "models":audit.models_checked,"static_models":audit.static_models,"faces":audit.faces_checked,
            "textures_present":audit.texture_references.len(),"empty_models":audit.empty_models,
            "zero_application_states":audit.zero_application_states})
        );
    }
}
