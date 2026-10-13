//! Own the connection reference that virt 0.4 does not release on drop.

use std::ops::Deref;

use virt::{connect::Connect as RawConnect, error::Error};

pub(crate) struct Connect(RawConnect);

impl Connect {
    pub(crate) fn open(uri: Option<&str>) -> Result<Self, Error> {
        RawConnect::open(uri).map(Self)
    }

    pub(crate) fn open_read_only(uri: Option<&str>) -> Result<Self, Error> {
        RawConnect::open_read_only(uri).map(Self)
    }
}

impl Deref for Connect {
    type Target = RawConnect;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Connect {
    fn drop(&mut self) {
        // Domain/network handles retain their own references and may outlive us.
        if let Err(error) = self.0.close() {
            tracing::warn!(%error, "failed to release libvirt connection");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use virt::domain::Domain;

    #[test]
    fn releases_writable_and_read_only_connection_references() {
        for open in [Connect::open, Connect::open_read_only] {
            let connection = open(Some("test:///default")).unwrap();
            let mut observer = connection.0.clone();
            drop(connection);
            assert_eq!(observer.close().unwrap(), 0);
        }
    }

    #[test]
    fn releases_connection_on_early_error() {
        let mut observer = None;
        let operation = |observer: &mut Option<RawConnect>| -> anyhow::Result<()> {
            let connection = Connect::open(Some("test:///default"))?;
            *observer = Some(connection.0.clone());
            anyhow::bail!("injected failure");
        };
        assert!(operation(&mut observer).is_err());
        assert_eq!(observer.unwrap().close().unwrap(), 0);
    }

    #[test]
    fn domain_remains_valid_after_connection_scope_ends() {
        let (domain, mut observer) = {
            let connection = Connect::open(Some("test:///default")).unwrap();
            let domain = Domain::lookup_by_name(&connection, "test").unwrap();
            (domain, connection.0.clone())
        };
        assert_eq!(domain.get_name().unwrap(), "test");
        drop(domain);
        assert_eq!(observer.close().unwrap(), 0);
    }
}
