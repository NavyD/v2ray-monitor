use std::sync::Arc;

use log::warn;
use regex::Regex;

use crate::{node::Node, v2ray::TcpPingStatistic};

use super::v2ray_task_config::FilterProperty;

pub trait Filter: Send + Sync {
    fn after_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        node_stats
    }

    fn before_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        node_stats
    }

    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

#[derive(Clone)]
pub struct FilterManager {
    filters: Arc<Vec<Box<dyn Filter>>>,
}

fn register<T: 'static + Filter>(filters: &mut Vec<Box<dyn Filter>>, filter: T) {
    filters.push(Box::new(filter));
}

impl FilterManager {
    pub fn new() -> Self {
        Self {
            filters: Arc::new(vec![]),
        }
    }

    pub fn with_ping(prop: FilterProperty) -> Self {
        let mut filters = vec![];
        if let Some(re) = prop.name_regex {
            register(&mut filters, NameRegexFilter::new(&[re]));
        }
        register(&mut filters, PingFilter {});
        Self {
            filters: Arc::new(vec![]),
        }
    }

    fn filter(
        &self,
        mut node_stats: Vec<(Node, TcpPingStatistic)>,
        bef_or_aft: bool,
    ) -> Vec<(Node, TcpPingStatistic)> {
        let old_len = node_stats.len();
        let msg = if bef_or_aft { "before" } else { "after" };
        let mut filtered_count = 0;
        for f in &*self.filters {
            let old_len = node_stats.len();
            log::trace!("{} filtering {} elements using {}", msg, old_len, f.name());
            node_stats = f.after_filter(node_stats);

            if node_stats.is_empty() {
                log::warn!("{} filters all elements", f.name());
                break;
            }

            let count = old_len - node_stats.len();
            filtered_count += count;
            log::trace!("{} {} filtered {} elements", msg, f.name(), count);
        }
        log::info!(
            "{} {} elements left, {} filters filter a total of {} elements",
            msg,
            node_stats.len() - old_len,
            self.filters.len(),
            filtered_count,
        );
        node_stats
    }
}

impl Filter for FilterManager {
    fn after_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        self.filter(node_stats, false)
    }

    fn before_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        self.filter(node_stats, true)
    }
}

pub struct NameRegexFilter {
    name_regexs: Vec<Regex>,
}

impl NameRegexFilter {
    pub fn new(name_regexs: &[String]) -> Self {
        if name_regexs.is_empty() {
            panic!("Empty name regexs");
        }
        Self {
            name_regexs: name_regexs
                .iter()
                .map(|name_regex| {
                    Regex::new(name_regex)
                        .unwrap_or_else(|e| panic!("regex `{}` error: {}", name_regex, e))
                })
                .collect(),
        }
    }
}

impl Filter for NameRegexFilter {
    fn before_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        node_stats
            .into_iter()
            .filter(|(n, _)| {
                self.name_regexs
                    .iter()
                    .any(|re| re.is_match(n.remark.as_ref().unwrap()))
            })
            .collect()
    }
}

pub struct PingFilter {}

impl PingFilter {}

impl Filter for PingFilter {
    fn after_filter(
        &self,
        mut node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        node_stats.sort_unstable_by(|(_, a), (_, b)| a.cmp(&b));
        node_stats
    }
}

pub struct SwitchFilter {}

impl SwitchFilter {}

impl Filter for SwitchFilter {
    fn after_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        todo!()
    }

    fn before_filter(
        &self,
        node_stats: Vec<(Node, TcpPingStatistic)>,
    ) -> Vec<(Node, TcpPingStatistic)> {
        todo!()
    }
}
