use crate::ipc::finch_ipc_capnp;

const MAX_JSON_VALUE_DEPTH: usize = 64;

pub fn encode_json_value(
    builder: finch_ipc_capnp::json_value::Builder<'_>,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    encode_json_value_at(builder, value, 0)
}

fn encode_json_value_at(
    mut builder: finch_ipc_capnp::json_value::Builder<'_>,
    value: &serde_json::Value,
    depth: usize,
) -> anyhow::Result<()> {
    if depth > MAX_JSON_VALUE_DEPTH {
        anyhow::bail!("dynamic value exceeds the maximum nesting depth");
    }
    match value {
        serde_json::Value::Null => builder.set_null_value(()),
        serde_json::Value::Bool(value) => builder.set_bool_value(*value),
        serde_json::Value::Number(value) if value.is_i64() => {
            builder.set_signed(value.as_i64().expect("checked signed JSON number"));
        }
        serde_json::Value::Number(value) if value.is_u64() => {
            builder.set_unsigned(value.as_u64().expect("checked unsigned JSON number"));
        }
        serde_json::Value::Number(value) => {
            builder.set_float(value.as_f64().expect("JSON number is representable as f64"));
        }
        serde_json::Value::String(value) => builder.set_text(value),
        serde_json::Value::Array(values) => {
            let mut encoded = builder.reborrow().init_array(values.len() as u32);
            for (index, value) in values.iter().enumerate() {
                encode_json_value_at(encoded.reborrow().get(index as u32), value, depth + 1)?;
            }
        }
        serde_json::Value::Object(values) => {
            let mut encoded = builder.reborrow().init_object(values.len() as u32);
            for (index, (name, value)) in values.iter().enumerate() {
                let mut field = encoded.reborrow().get(index as u32);
                field.set_name(name);
                encode_json_value_at(field.reborrow().init_value(), value, depth + 1)?;
            }
        }
    }
    Ok(())
}

pub fn decode_json_value(
    reader: finch_ipc_capnp::json_value::Reader<'_>,
) -> anyhow::Result<serde_json::Value> {
    decode_json_value_at(reader, 0)
}

fn decode_json_value_at(
    reader: finch_ipc_capnp::json_value::Reader<'_>,
    depth: usize,
) -> anyhow::Result<serde_json::Value> {
    if depth > MAX_JSON_VALUE_DEPTH {
        anyhow::bail!("dynamic value exceeds the maximum nesting depth");
    }
    use finch_ipc_capnp::json_value::Which;
    Ok(match reader.which()? {
        Which::NullValue(()) => serde_json::Value::Null,
        Which::BoolValue(value) => serde_json::Value::Bool(value),
        Which::Signed(value) => serde_json::Value::Number(value.into()),
        Which::Unsigned(value) => serde_json::Value::Number(value.into()),
        Which::Float(value) => serde_json::Value::Number(
            serde_json::Number::from_f64(value)
                .ok_or_else(|| anyhow::anyhow!("dynamic value contains a non-finite float"))?,
        ),
        Which::Text(value) => serde_json::Value::String(value?.to_str()?.to_owned()),
        Which::Array(values) => serde_json::Value::Array(
            values?
                .iter()
                .map(|value| decode_json_value_at(value, depth + 1))
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        Which::Object(fields) => {
            let mut values = serde_json::Map::new();
            for field in fields?.iter() {
                values.insert(
                    field.get_name()?.to_str()?.to_owned(),
                    decode_json_value_at(field.get_value()?, depth + 1)?,
                );
            }
            serde_json::Value::Object(values)
        }
    })
}
