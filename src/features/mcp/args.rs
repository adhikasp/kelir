//! The `kelir mcp` command line: parsing the words, typing `k=v` values and checking the arguments
//! against the tool's `inputSchema`.
//! The words are parsed by hand, not by clap: after `kelir mcp <server> <tool>` the flags
//! (`--json`, `--schema`, `--envelope`) belong to the call, and `--json` means "the arguments, as
//! JSON" there but "machine-readable" after `kelir mcp ls`.

use serde_json::{Map, Value};

/// How much of `<server> ls` to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Name,
    Desc,
    Full,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `kelir mcp ls [--json]`
    Servers { json: bool },
    /// `kelir mcp <server> ls [--detail …]`
    Tools { server: String, detail: Detail },
    /// `kelir mcp <server> <tool> …`
    Call(CallArgs),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallArgs {
    pub server: String,
    pub tool: String,
    /// `k=v` pairs, in order, unparsed (typing needs the schema).
    pub pairs: Vec<(String, String)>,
    /// The `--json` object (or `-` for stdin, resolved by the caller).
    pub json: Option<String>,
    pub schema: bool,
    pub envelope: bool,
}

pub const USAGE: &str = "\
usage: kelir mcp ls [--json]
       kelir mcp <server> ls [--detail name|desc|full]
       kelir mcp <server> <tool> [k=v ...] [--json '{...}'] [--schema] [--envelope]
       kelir mcp <server> call <tool> ...   (for a tool named ls or call)";

/// Parse the words after `kelir mcp`.
pub fn parse(words: &[String]) -> Result<Command, String> {
    let Some(first) = words.first() else {
        return Err("missing arguments".into());
    };
    if first == "ls" {
        let mut json = false;
        for word in &words[1..] {
            match word.as_str() {
                "--json" => json = true,
                other => return Err(format!("`kelir mcp ls` does not take `{other}`")),
            }
        }
        return Ok(Command::Servers { json });
    }
    if first.starts_with('-') {
        return Err(format!("unknown flag `{first}`"));
    }
    let server = first.clone();
    let Some(second) = words.get(1) else {
        return Err(format!(
            "missing tool: `kelir mcp {server} ls` lists the tools"
        ));
    };
    if second == "ls" {
        let mut detail = Detail::Desc;
        let mut rest = words[2..].iter();
        while let Some(word) = rest.next() {
            let value = if word == "--detail" {
                rest.next()
                    .ok_or("`--detail` needs name, desc or full")?
                    .as_str()
            } else if let Some(value) = word.strip_prefix("--detail=") {
                value
            } else {
                return Err(format!("`kelir mcp {server} ls` does not take `{word}`"));
            };
            detail = match value {
                "name" => Detail::Name,
                "desc" => Detail::Desc,
                "full" => Detail::Full,
                other => return Err(format!("`--detail {other}`: use name, desc or full")),
            };
        }
        return Ok(Command::Tools { server, detail });
    }
    if second.starts_with('-') {
        return Err(format!("expected a tool name, got `{second}`"));
    }
    // `kelir mcp <server> call <tool> ...` reaches a tool that is itself named
    // `ls` (or `call`, with `call call`), which the bare form would list.
    let (tool, skip) = if second == "call" {
        match words.get(2) {
            Some(tool) if !tool.starts_with('-') => (tool.clone(), 3),
            _ => return Err("`call` needs a tool name".into()),
        }
    } else {
        (second.clone(), 2)
    };
    let mut call = CallArgs {
        server,
        tool,
        pairs: Vec::new(),
        json: None,
        schema: false,
        envelope: false,
    };
    let mut rest = words[skip..].iter();
    while let Some(word) = rest.next() {
        match word.as_str() {
            "--schema" => call.schema = true,
            "--envelope" => call.envelope = true,
            "--json" => {
                let value = rest.next().ok_or("`--json` needs a JSON object (or `-`)")?;
                call.json = Some(value.clone());
            }
            other if other.starts_with("--json=") => {
                call.json = Some(other["--json=".len()..].to_string());
            }
            other if other.starts_with("--") && !other.contains('=') => {
                return Err(format!("unknown flag `{other}`"));
            }
            other => match other.split_once('=') {
                Some((key, value)) if !key.is_empty() && !key.starts_with('-') => {
                    call.pairs.push((key.to_string(), value.to_string()));
                }
                _ => {
                    return Err(format!(
                        "expected `key=value`, got `{other}` (use `--json '{{...}}'` for \
                         structured arguments)"
                    ));
                }
            },
        }
    }
    Ok(Command::Call(call))
}

/// The scalar type names a property schema allows: its `type` (a string or a
/// list) and the `type`s of its `anyOf`/`oneOf` branches, recursively.
fn declared_types(property: &Value, out: &mut Vec<String>) {
    match property.get("type") {
        Some(Value::String(name)) => out.push(name.clone()),
        Some(Value::Array(list)) => {
            out.extend(list.iter().filter_map(Value::as_str).map(String::from))
        }
        _ => {}
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(branches)) = property.get(key) {
            for branch in branches {
                declared_types(branch, out);
            }
        }
    }
}

/// What a tool declares for one property: the list of type names (empty when
/// it declares none).
fn declared_type(schema: &Value, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(property) = schema.get("properties").and_then(|p| p.get(key)) {
        declared_types(property, &mut out);
    }
    out
}

/// Type one `k=v` value.
pub fn type_value(raw: &str, declared: &[String]) -> Value {
    let accepts_string = declared.iter().any(|name| name == "string");
    let others: Vec<&String> = declared
        .iter()
        .filter(|name| !matches!(name.as_str(), "string" | "null"))
        .collect();
    if accepts_string && others.is_empty() {
        return Value::String(raw.to_string());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => {
            if integer_overflows(raw, &value) {
                return Value::String(raw.to_string());
            }
            if accepts_string && !others.iter().any(|name| type_matches(name, &value)) {
                return Value::String(raw.to_string());
            }
            value
        }
        Err(_) => Value::String(raw.to_string()),
    }
}

/// `raw` is integer text that a JSON number here would round (beyond i64/u64).
fn integer_overflows(raw: &str, value: &Value) -> bool {
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    value.is_f64() && !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// The call's argument object: the `--json` object first, then each `k=v`
/// (which wins). `schema` is the tool's `inputSchema`, or `Null` when unknown.
pub fn build(
    pairs: &[(String, String)],
    json: Option<&str>,
    schema: &Value,
) -> Result<Value, String> {
    let mut object = match json {
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Err("`--json` must be a JSON object".into()),
            Err(error) => return Err(format!("`--json` is not valid JSON: {error}")),
        },
        None => Map::new(),
    };
    for (key, raw) in pairs {
        let declared = declared_type(schema, key);
        object.insert(key.clone(), type_value(raw, &declared));
    }
    Ok(Value::Object(object))
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value.as_f64().is_some_and(|float| float.fract() == 0.0)
        }
        other => type_name(value) == other,
    }
}

fn shown(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 60 {
        let cut: String = text.chars().take(57).collect();
        format!("{cut}...")
    } else {
        text
    }
}

/// Check `value` against the supported subset of JSON Schema: `type`, `enum`, `required`, `properties`,
/// `additionalProperties: false`, `items`, `minimum`/`maximum`.
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    let mut problems = Vec::new();
    check(schema, value, "", &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn at(path: &str) -> String {
    if path.is_empty() {
        "arguments".into()
    } else {
        format!("`{path}`")
    }
}

fn check(schema: &Value, value: &Value, path: &str, problems: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Some(expected) = schema.get("type") {
        let names: Vec<&str> = match expected {
            Value::String(name) => vec![name.as_str()],
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !names.is_empty() && !names.iter().any(|name| type_matches(name, value)) {
            problems.push(format!(
                "{}: expected {}, got {} {}",
                at(path),
                names.join(" or "),
                type_name(value),
                shown(value)
            ));
            return;
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(branches)) = schema.get(key)
            && !branches.is_empty()
            && !branches.iter().any(|branch| {
                let mut own = Vec::new();
                check(branch, value, path, &mut own);
                own.is_empty()
            })
        {
            problems.push(format!(
                "{}: {} matches none of the `{key}` alternatives",
                at(path),
                shown(value)
            ));
            return;
        }
    }
    if let Some(Value::Array(allowed)) = schema.get("enum")
        && !allowed.contains(value)
    {
        let list: Vec<String> = allowed.iter().map(shown).collect();
        problems.push(format!(
            "{}: {} is not one of {}",
            at(path),
            shown(value),
            list.join(", ")
        ));
    }
    if let (Some(number), Some(min)) = (
        value.as_f64(),
        schema.get("minimum").and_then(Value::as_f64),
    ) && number < min
    {
        problems.push(format!("{}: {number} is below the minimum {min}", at(path)));
    }
    if let (Some(number), Some(max)) = (
        value.as_f64(),
        schema.get("maximum").and_then(Value::as_f64),
    ) && number > max
    {
        problems.push(format!("{}: {number} is above the maximum {max}", at(path)));
    }
    if let Some(object) = value.as_object() {
        if let Some(Value::Array(required)) = schema.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    problems.push(format!("{}: missing required `{name}`", at(path)));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            match properties.and_then(|map| map.get(key)) {
                Some(child_schema) => check(child_schema, child, &child_path, problems),
                None => {
                    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                        let known: Vec<&str> = properties
                            .map(|map| map.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        problems.push(format!(
                            "{}: unknown argument `{key}` (known: {})",
                            at(path),
                            known.join(", ")
                        ));
                    }
                }
            }
        }
    }
    if let (Some(items), Some(list)) = (schema.get("items"), value.as_array()) {
        for (index, item) in list.iter().enumerate() {
            check(items, item, &format!("{path}[{index}]"), problems);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn usage_errors_say_what_was_wrong_and_call_reaches_named_tools() {
        for (text, needle) in [
            ("", "missing"),
            ("demo", "missing tool"),
            ("--wat", "unknown flag"),
            ("ls extra", "does not take"),
            ("demo ls --detail loud", "name, desc or full"),
            ("demo ls --detail", "needs"),
        ] {
            let error = parse(&words(text)).unwrap_err();
            assert!(error.contains(needle), "{text:?}: {error}");
        }
        let Command::Call(call) = parse(&words("demo call ls")).unwrap() else {
            panic!("a call")
        };
        assert_eq!(call.tool, "ls");
    }

    #[test]
    fn the_three_shapes_parse() {
        assert_eq!(
            parse(&words("ls")).unwrap(),
            Command::Servers { json: false }
        );
        assert_eq!(
            parse(&words("ls --json")).unwrap(),
            Command::Servers { json: true }
        );
        assert_eq!(
            parse(&words("demo ls")).unwrap(),
            Command::Tools {
                server: "demo".into(),
                detail: Detail::Desc
            }
        );
        assert_eq!(
            parse(&words("demo ls --detail full")).unwrap(),
            Command::Tools {
                server: "demo".into(),
                detail: Detail::Full
            }
        );
        let Command::Call(call) = parse(&words("demo echo text=hi n=3 --envelope")).unwrap() else {
            panic!("a call")
        };
        assert_eq!(call.tool, "echo");
        assert_eq!(
            call.pairs,
            vec![("text".into(), "hi".into()), ("n".into(), "3".into())]
        );
        assert!(call.envelope && !call.schema && call.json.is_none());
    }

    #[test]
    fn json_means_arguments_after_a_tool_and_format_after_ls() {
        let Command::Call(call) = parse(&words("demo echo --json {}")).unwrap() else {
            panic!("a call")
        };
        assert_eq!(call.json.as_deref(), Some("{}"));
        let Command::Call(call) =
            parse(&["demo".into(), "echo".into(), "--json={\"a\":1}".into()]).unwrap()
        else {
            panic!("a call")
        };
        assert_eq!(call.json.as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn k_v_values_are_json_when_they_can_be_else_strings() {
        assert_eq!(type_value("3", &[]), json!(3));
        assert_eq!(type_value("2.5", &[]), json!(2.5));
        assert_eq!(type_value("true", &[]), json!(true));
        assert_eq!(type_value("null", &[]), json!(null));
        assert_eq!(type_value("[1,2]", &[]), json!([1, 2]));
        assert_eq!(type_value("{\"a\":1}", &[]), json!({"a": 1}));
        assert_eq!(type_value("hello world", &[]), json!("hello world"));
        assert_eq!(type_value("007x", &[]), json!("007x"));
        // A declared string keeps the text exactly as typed.
        assert_eq!(type_value("3", &["string".to_string()]), json!("3"));
        assert_eq!(type_value("true", &["string".to_string()]), json!("true"));
    }

    #[test]
    fn nullable_and_union_strings_keep_their_text() {
        let schema = json!({"properties": {
            "a": {"anyOf": [{"type": "string"}, {"type": "null"}]},
            "b": {"type": ["string", "null"]},
            "u": {"type": ["string", "integer"]},
            "n": {"type": "integer"}}});
        let args = build(
            &[
                ("a".into(), "123".into()),
                ("b".into(), "123".into()),
                ("u".into(), "123".into()),
                ("n".into(), "123".into()),
            ],
            None,
            &schema,
        )
        .unwrap();
        assert_eq!(args, json!({"a": "123", "b": "123", "u": 123, "n": 123}));
        assert_eq!(
            type_value("hi", &["string".into(), "integer".into()]),
            json!("hi")
        );
    }

    #[test]
    fn a_huge_integer_stays_text() {
        let big = "123456789012345678901234567890";
        assert_eq!(type_value(big, &[]), json!(big));
        assert_eq!(
            type_value("-123456789012345678901234567890", &[]),
            json!("-123456789012345678901234567890")
        );
        assert_eq!(
            type_value("18446744073709551615", &[]),
            json!(18446744073709551615u64)
        );
        assert_eq!(type_value("1.5e3", &[]), json!(1500.0));
    }

    #[test]
    fn build_merges_json_then_pairs_using_the_schema() {
        let schema = json!({"properties": {"id": {"type": "string"}, "n": {"type": "integer"}}});
        let args = build(
            &[("id".into(), "42".into()), ("n".into(), "7".into())],
            Some("{\"n\": 1, \"extra\": true}"),
            &schema,
        )
        .unwrap();
        assert_eq!(args, json!({"id": "42", "n": 7, "extra": true}));
        assert!(
            build(&[], Some("[1]"), &schema)
                .unwrap_err()
                .contains("object")
        );
        assert!(
            build(&[], Some("{"), &schema)
                .unwrap_err()
                .contains("not valid")
        );
    }

    #[test]
    fn schema_validation_accepts_unknown_shapes_and_reports_paths() {
        // A server may publish a schema shape we do not interpret: accept the value rather than
        // refuse a valid tool call.
        assert!(validate(&Value::Null, &json!({"anything": 1})).is_ok());
        assert!(validate(&json!({}), &json!({"anything": 1})).is_ok());
        assert!(validate(&json!({"type": ["string", "null"]}), &json!(null)).is_ok());
        let schema = json!({"anyOf": [{"type": "string"}, {"type": "null"}]});
        assert!(validate(&schema, &json!("x")).is_ok());
        assert!(validate(&schema, &json!(null)).is_ok());
        assert!(validate(&schema, &json!(3)).unwrap_err().contains("anyOf"));
        let schema = json!({
            "type": "object", "required": ["text"], "additionalProperties": false,
            "properties": {
                "text": {"type": "string"},
                "n": {"type": "integer", "minimum": 1, "maximum": 5},
                "mode": {"enum": ["a", "b"]},
                "tags": {"type": "array", "items": {"type": "string"}}
            }
        });
        let error = validate(
            &schema,
            &json!({"n": "two", "mode": "c", "tags": ["x", 3], "bogus": 1}),
        )
        .unwrap_err();
        for needle in [
            "missing required `text`",
            "`n`: expected integer, got string",
            "`mode`: \"c\" is not one of \"a\", \"b\"",
            "`tags[1]`: expected string, got integer",
            "unknown argument `bogus` (known:",
        ] {
            assert!(error.contains(needle), "{needle}\n{error}");
        }
        assert!(
            validate(&schema, &json!({"text": "x", "n": 0}))
                .unwrap_err()
                .contains("below the minimum")
        );
        assert!(
            validate(&schema, &json!({"text": "x", "n": 9}))
                .unwrap_err()
                .contains("above the maximum")
        );
    }
}
