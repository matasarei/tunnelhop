//! The installed bundles as the window lists them (SPEC §3.5): the service's answer to
//! `ListBundles` and `DeleteBundle`, which both come back as the list.

use serde::Serialize;
use th_core::ipc::{BundleInfo, Response};

use crate::lang::text;

/// Every installed bundle, ordered by name, and the id of the active one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BundleList {
    pub bundles: Vec<BundleInfo>,
    pub active: Option<String>,
}

/// The service's answer as the list, or an error to show.
pub fn list(response: Response) -> Result<BundleList, String> {
    match response {
        Response::Bundles { bundles, active } => Ok(BundleList { bundles, active }),
        Response::Error { message } => Err(message),
        other => Err(text(
            "client.unexpected",
            &[("answer", &format!("{other:?}"))],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_services_answers_become_the_list_or_an_error() {
        let bundle = BundleInfo {
            bundle_id: "0123456789abcdef".into(),
            bundle_name: "Lab".into(),
            profiles: Vec::new(),
        };
        assert_eq!(
            list(Response::Bundles {
                bundles: vec![bundle.clone()],
                active: Some("0123456789abcdef".into()),
            }),
            Ok(BundleList {
                bundles: vec![bundle],
                active: Some("0123456789abcdef".into()),
            })
        );
        assert_eq!(
            list(Response::Error {
                message: "bundle 0123456789abcdef is not installed".into()
            }),
            Err("bundle 0123456789abcdef is not installed".into())
        );
        assert!(list(Response::Ok).is_err(), "not an answer to a listing");
    }
}
