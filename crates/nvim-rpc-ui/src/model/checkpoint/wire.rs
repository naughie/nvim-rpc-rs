use super::{CheckpointError, State};

use postcard::Error;
use postcard::ser_flavors::Flavor;

pub(super) fn verify_payload(state: &State, payload: &[u8]) -> Result<(), CheckpointError> {
    struct Compare<'a>(&'a [u8]);

    impl Flavor for Compare<'_> {
        type Output = ();

        fn try_push(&mut self, byte: u8) -> Result<(), Error> {
            self.try_extend(&[byte])
        }

        fn try_extend(&mut self, bytes: &[u8]) -> Result<(), Error> {
            self.0 = self.0.strip_prefix(bytes).ok_or(Error::SerdeSerCustom)?;
            Ok(())
        }

        fn finalize(self) -> Result<(), Error> {
            if self.0.is_empty() {
                Ok(())
            } else {
                Err(Error::SerdeSerCustom)
            }
        }
    }

    // Require the encoder's representation, including unique keys in state maps.
    // Value::Map stores its entries as an ordered sequence and preserves duplicates.
    postcard::serialize_with_flavor(state, Compare(payload))
        .map_err(|_| CheckpointError::Invalid("payload representation"))
}

pub(in crate::model) mod handle {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(in crate::model) fn serialize<T, S>(id: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Copy + Into<i64>,
        S: Serializer,
    {
        (*id).into().serialize(serializer)
    }

    pub(in crate::model) fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: From<i64>,
        D: Deserializer<'de>,
    {
        i64::deserialize(deserializer).map(T::from)
    }
}

pub(in crate::model) mod optional_handle {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(in crate::model) fn serialize<T, S>(
        id: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        T: Copy + Into<i64>,
        S: Serializer,
    {
        id.map(Into::into).serialize(serializer)
    }

    pub(in crate::model) fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        T: From<i64>,
        D: Deserializer<'de>,
    {
        Option::<i64>::deserialize(deserializer).map(|id| id.map(T::from))
    }
}

pub(in crate::model) mod bytes {
    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};

    use std::fmt;
    use std::sync::Arc;

    pub(in crate::model) fn serialize<S: Serializer>(
        bytes: &[u8],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub(in crate::model) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<[u8]>, D::Error> {
        struct Bytes;

        impl Visitor<'_> for Bytes {
            type Value = Arc<[u8]>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("checkpoint byte string")
            }

            fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                Ok(Arc::from(bytes))
            }
        }

        deserializer.deserialize_bytes(Bytes)
    }
}
