use std::collections::{BTreeMap, btree_map::Entry};

use serde::de::DeserializeOwned;
use serde_json::value::{RawValue, to_raw_value};
use snafu::ResultExt;

use crate::{
    Field, Result,
    error::{ParseUnknownObjectSnafu, SerializeUnknownObjectSnafu},
};

pub(crate) fn field<T>(
    fields: &mut BTreeMap<String, Box<RawValue>>,
    name: &'static str,
    field: &'static str,
) -> Field<T>
where
    T: DeserializeOwned,
{
    Field::decode(fields.remove(name), field)
}

// Only unknown metadata is merged as raw objects. Scalars and arrays remain
// opaque, and no recursive JSON value tree is retained between merges.
pub(crate) fn merge(
    target: &mut BTreeMap<String, Box<RawValue>>,
    source: BTreeMap<String, Box<RawValue>>,
) -> Result<()> {
    for (name, value) in source {
        match target.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(mut entry) => {
                let old = entry.get().get();
                let new = value.get();
                if old.trim_start().starts_with('{') && new.trim_start().starts_with('{') {
                    let mut fields =
                        serde_json::from_str(old).with_context(|_| ParseUnknownObjectSnafu { field: entry.key() })?;
                    let source =
                        serde_json::from_str(new).with_context(|_| ParseUnknownObjectSnafu { field: entry.key() })?;
                    merge(&mut fields, source)?;
                    let value =
                        to_raw_value(&fields).with_context(|_| SerializeUnknownObjectSnafu { field: entry.key() })?;
                    entry.insert(value);
                } else {
                    entry.insert(value);
                }
            }
        }
    }
    Ok(())
}
