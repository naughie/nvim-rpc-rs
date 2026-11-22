use super::MAX_VALUE_DEPTH;
use super::wire;

use crate::model::Value;

use serde::de::{DeserializeSeed, EnumAccess, Error as _, SeqAccess, VariantAccess, Visitor};
use serde::{Deserialize, Deserializer};

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Seed(0).deserialize(deserializer)
    }
}

// The order and names match the variants of Value in the checkpoint schema.
#[derive(Deserialize)]
#[serde(variant_identifier)]
enum Variant {
    Nil,
    Bool,
    Int,
    Uint,
    Float,
    String,
    Binary,
    Array,
    Map,
    Extension,
}

#[derive(Deserialize)]
struct Bytes(#[serde(deserialize_with = "wire::bytes::deserialize")] Arc<[u8]>);

#[derive(Clone, Copy)]
struct Seed(usize);

impl<'de> DeserializeSeed<'de> for Seed {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        // Value is the only recursive type in the checkpoint. Check depth
        // before descending so malformed records cannot grow the decode stack.
        if self.0 > MAX_VALUE_DEPTH {
            return Err(D::Error::custom("checkpoint value depth exceeded"));
        }
        const VARIANTS: &[&str] = &[
            "Nil",
            "Bool",
            "Int",
            "Uint",
            "Float",
            "String",
            "Binary",
            "Array",
            "Map",
            "Extension",
        ];
        deserializer.deserialize_enum("Value", VARIANTS, self)
    }
}

impl<'de> Visitor<'de> for Seed {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a retained UI value")
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Value, A::Error> {
        let (variant, payload) = data.variant()?;
        let child = Self(self.0.saturating_add(1));
        match variant {
            Variant::Nil => payload.unit_variant().map(|()| Value::Nil),
            Variant::Bool => payload.newtype_variant().map(Value::Bool),
            Variant::Int => payload.newtype_variant().map(Value::Int),
            Variant::Uint => payload.newtype_variant().map(Value::Uint),
            Variant::Float => payload.newtype_variant().map(Value::Float),
            Variant::String => payload.newtype_variant().map(|Bytes(v)| Value::String(v)),
            Variant::Binary => payload.newtype_variant().map(|Bytes(v)| Value::Binary(v)),
            Variant::Array => payload
                .newtype_variant_seed(Sequence(child))
                .map(Value::Array),
            Variant::Map => payload
                .newtype_variant_seed(Sequence(Pair(child, child)))
                .map(Value::Map),
            Variant::Extension => {
                let (tag, Bytes(bytes)) =
                    payload.tuple_variant(2, Pair(PhantomData, PhantomData))?;
                Ok(Value::Extension(tag, bytes))
            }
        }
    }
}

struct Sequence<S>(S);

impl<'de, S: DeserializeSeed<'de> + Copy> DeserializeSeed<'de> for Sequence<S> {
    type Value = Arc<[S::Value]>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, S: DeserializeSeed<'de> + Copy> Visitor<'de> for Sequence<S> {
    type Value = Arc<[S::Value]>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence of retained UI values")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(self.0)? {
            values.push(value);
        }
        Ok(values.into())
    }
}

#[derive(Clone, Copy)]
struct Pair<L, R>(L, R);

impl<'de, L: DeserializeSeed<'de>, R: DeserializeSeed<'de>> DeserializeSeed<'de> for Pair<L, R> {
    type Value = (L::Value, R::Value);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_tuple(2, self)
    }
}

impl<'de, L: DeserializeSeed<'de>, R: DeserializeSeed<'de>> Visitor<'de> for Pair<L, R> {
    type Value = (L::Value, R::Value);

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a pair of values")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let left = seq
            .next_element_seed(self.0)?
            .ok_or_else(|| A::Error::custom("missing first value"))?;
        let right = seq
            .next_element_seed(self.1)?
            .ok_or_else(|| A::Error::custom("missing second value"))?;
        Ok((left, right))
    }
}
