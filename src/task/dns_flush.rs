// Copyright (c) 2014, 2015 Robert Clipsham <robert@octarineparrot.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// This example shows a basic packet logger using libpnet
extern crate pnet;

use dns_lookup::lookup_host;

use anyhow::{anyhow, Result};
use std::net::IpAddr;

use tokio::{sync::mpsc::Sender, time::interval};

use super::{v2ray_task_config::DnsFlushProperty, RetryService};

pub struct HostDnsFlushTask {
    prop: DnsFlushProperty,
    retry_srv: RetryService,
}

impl HostDnsFlushTask {
    pub fn new(prop: DnsFlushProperty) -> Self {
        Self {
            retry_srv: RetryService::new(prop.retry.clone()),
            prop,
        }
    }

    pub async fn run(&self, tx: Sender<Vec<IpAddr>>) -> Result<()> {
        let mut interval = interval(self.prop.update_interval);
        loop {
            interval.tick().await;
            match self.retry_srv.retry_on(|| self.update(&tx), false).await {
                Ok(a) => {
                    log::debug!("dns flush task successfully duration: {:?}", a.1);
                }
                Err(e) => {
                    log::error!("dns flush retry all error: {}", e);
                }
            }
        }
    }

    async fn update(&self, tx: &Sender<Vec<IpAddr>>) -> Result<()> {
        log::trace!("updating dns for hosts: {:?}", self.prop.hosts);
        let ips = self
            .prop
            .hosts
            .iter()
            .flat_map(|host| lookup_host(host))
            .flatten()
            .collect::<Vec<_>>();
        if ips.is_empty() {
            log::warn!(
                "dns resolve error: not found any ips for hosts: {:?}",
                self.prop.hosts
            );
            return Err(anyhow!("not found any ips"));
        }
        log::trace!("sending updated ips len: {}, {:?}", ips.len(), ips);
        tx.send(ips).await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{sync::mpsc::channel, time::timeout};

    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let prop = DnsFlushProperty {
            hosts: vec![
                "www.google.com".to_string(),
                "www.youtube.com".to_string(),
                "github.com".to_string(),
            ],
            ..Default::default()
        };
        let task = HostDnsFlushTask::new(prop);
        let (tx, mut rx) = channel::<Vec<IpAddr>>(1);
        timeout(Duration::from_secs(4), task.update(&tx)).await??;
        let v = rx.recv().await.unwrap();
        assert!(!v.is_empty());
        Ok(())
    }
}
