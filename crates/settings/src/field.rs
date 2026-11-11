use std::sync::Arc;

use serde::{Serialize, Serializer, de::DeserializeOwned, ser::Error as _};
use serde_json::value::RawValue;
use snafu::ResultExt;

use crate::{
    Result,
    error::{InvalidSettingSnafu, MissingSettingSnafu},
};

/// A typed configuration override. Invalid lower-priority fields may be
/// replaced before validation, matching the extension's
/// initialization/workspace layering.
#[derive(Clone, Debug)]
pub struct Field<T>(State<T>);

#[derive(Clone, Debug)]
enum State<T> {
    Missing,
    Present(T),
    Invalid {
        field: &'static str,
        source: Arc<serde_json::Error>,
    },
}

impl<T> Default for Field<T> {
    fn default() -> Self {
        Self(State::Missing)
    }
}

impl<T> Field<T> {
    pub fn new(value: T) -> Self {
        Self(State::Present(value))
    }

    pub fn is_missing(&self) -> bool {
        matches!(&self.0, State::Missing)
    }

    pub fn get(&self) -> Option<&T> {
        match &self.0 {
            State::Present(value) => Some(value),
            State::Missing | State::Invalid { .. } => None,
        }
    }

    pub fn into_value(self) -> Result<Option<T>> {
        match self.0 {
            State::Missing => Ok(None),
            State::Present(value) => Ok(Some(value)),
            State::Invalid { field, source } => Err(source).context(InvalidSettingSnafu { field }),
        }
    }

    pub(crate) fn decode(value: Option<Box<RawValue>>, field: &'static str) -> Self
    where
        T: DeserializeOwned,
    {
        match value {
            None => Self::default(),
            Some(value) => match serde_json::from_str(value.get()) {
                Ok(value) => Self::new(value),
                Err(source) => Self(State::Invalid {
                    field,
                    source: Arc::new(source),
                }),
            },
        }
    }

    pub(crate) fn replace(&mut self, source: Self) {
        if !source.is_missing() {
            *self = source;
        }
    }

    pub(crate) fn merge(&mut self, source: Self, merge: impl FnOnce(&mut T, T) -> Result<()>) -> Result<()> {
        match (&mut self.0, source.0) {
            (_, State::Missing) => {}
            (State::Present(target), State::Present(source)) => merge(target, source)?,
            (target, source) => *target = source,
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if let State::Invalid { field, source } = &self.0 {
            return Err(Arc::clone(source)).context(InvalidSettingSnafu { field: *field });
        }
        Ok(())
    }
}

impl<T: Serialize> Serialize for Field<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            State::Present(value) => value.serialize(serializer),
            State::Missing => Err(S::Error::custom(MissingSettingSnafu.build())),
            State::Invalid { field, source } => Err(S::Error::custom(format_args!("{field}: {source}"))),
        }
    }
}
