//! herdr's ids, one type each so a tab id can't go where a pane id belongs.
//!
//! Each is a cheap-to-clone `Arc<str>` that (de)serializes as a plain string
//! and borrows as `str`, so maps keyed by an id still look up by `&str`.
//! It derefs to `str` for reading (comparing, formatting, `Option::as_deref`);
//! anything that acts on an id takes the typed id, never `&str`.
//! Ids are only unique within one herdr server.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

macro_rules! ids {
    ($($(#[$doc:meta])* $name:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Arc<str>);

        impl $name {
            pub fn new(id: impl Into<Arc<str>>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(&*self.0, f)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self(id.into())
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id.into())
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                &*self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                &*self.0 == *other
            }
        }
    )*};
}

ids! {
    /// herdr's pane id, e.g. `w1:p3`.
    PaneId;
    /// herdr's tab id, e.g. `w1:t1`.
    TabId;
    /// herdr's workspace id, e.g. `w1`.
    WorkspaceId;
    /// The terminal running in a pane, e.g. `term_1`; what
    /// `herdr terminal attach` takes. It outlives a pane move.
    TerminalId;
    /// A saved machine's profile id (`herdr machine list`).
    ProfileId;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn an_id_should_serialize_as_a_plain_string() {
        let json = serde_json::to_string(&PaneId::from("w1:p3")).unwrap();
        assert_eq!(json, r#""w1:p3""#);
    }

    #[test]
    fn an_id_should_deserialize_from_a_plain_string() {
        let id: TabId = serde_json::from_str(r#""w1:t1""#).unwrap();
        assert_eq!(id, "w1:t1");
    }

    #[test]
    fn a_map_keyed_by_id_should_look_up_by_str() {
        let map = BTreeMap::from([(PaneId::from("w1:p1"), 1)]);
        assert_eq!(map.get("w1:p1"), Some(&1));
    }
}
