//! Where this build of the app comes from, as an interface: whether the person owns
//! it (checked at launch, where the store asks for that) and where a newer version
//! comes from. A build for a store has that store's implementation (feature
//! `store`); one from GitHub or for development has [`NoStore`]. Another store is
//! another implementation.

#[cfg(all(target_os = "android", feature = "store"))]
mod meta;

/// What the ownership check says so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreCheck {
    /// No answer yet.
    Waiting,
    Owned,
    /// Not owned, or the check could not run: the person is told so (naming the
    /// store) before the app closes.
    NotOwned {
        /// The store's own name.
        store: &'static str,
    },
}

/// Where a newer version of the app comes from: what to do (the message that says
/// it), and where (shown large: the store's name, or an address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateHint {
    pub how: &'static str,
    pub place: &'static str,
}

pub trait Store {
    /// The ownership check, asked once per pass of the loop until the app closes.
    fn poll(&mut self) -> StoreCheck;
    /// Where to get a newer version of the app, for the screen that says to update it.
    fn update_hint(&self) -> UpdateHint;
}

/// A build from GitHub, or for development: nothing to check, and new versions are on
/// the releases page.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStore;

impl Store for NoStore {
    fn poll(&mut self) -> StoreCheck {
        StoreCheck::Owned
    }

    fn update_hint(&self) -> UpdateHint {
        UpdateHint {
            how: "store-update-apk",
            place: "github.com/OlexiyOdarchuk/prostir/releases",
        }
    }
}

/// The store of this build, its check started (a store may want it within seconds of
/// launch).
#[cfg(target_os = "android")]
pub fn of_this_build(app: &android_activity::AndroidApp) -> Box<dyn Store> {
    #[cfg(feature = "store")]
    {
        Box::new(meta::MetaStore::start(app))
    }
    #[cfg(not(feature = "store"))]
    {
        let _ = app;
        Box::new(NoStore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_store_has_nothing_to_check() {
        let mut store = NoStore;
        assert_eq!(store.poll(), StoreCheck::Owned);
        let hint = store.update_hint();
        assert_eq!(hint.place, "github.com/OlexiyOdarchuk/prostir/releases");
        assert_eq!(
            crate::lang::builtin("en").t(hint.how),
            "Install the new APK from"
        );
        assert_eq!(
            crate::lang::builtin("uk").t(hint.how),
            "Встановіть новий APK з"
        );
    }
}
