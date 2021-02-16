use std::sync::Arc;

use crate::{task::switch::*, v2ray::node::Node};

use regex::Regex;

pub trait Filter<T, R>: Send + Sync {
    fn filter(&self, data: T) -> R;

    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}
#[derive(Clone)]
pub struct FilterManager<T, R> {
    filters: Arc<Vec<Box<dyn Filter<T, R>>>>,
}

pub struct SwitchSelectFilter {
    size: usize,
}

impl SwitchSelectFilter {
    pub fn new(size: usize) -> Self {
        Self { size }
    }
}

impl Filter<SwitchData, Vec<SwitchNodeStat>> for SwitchSelectFilter {
    fn filter(&self, data: SwitchData) -> Vec<SwitchNodeStat> {
        let mut val = data.lock();
        let mut selected = vec![];
        for _ in 0..self.size {
            if let Some(v) = val.pop() {
                selected.push(v);
            }
        }
        if self.size == 1 {
            return selected;
        }

        let mut max_count = 1;
        let mut host = selected.first().unwrap().node.host.clone();
        for ns1 in &selected {
            let mut count = 0;
            for ns2 in &selected {
                if ns1.node.host == ns2.node.host {
                    count += 1;
                }
            }
            if max_count < count {
                host = ns1.node.host.clone();
                max_count = count;
            }
        }
        log::debug!(
            "selected load balance host: {:?}, count: {}",
            host,
            max_count
        );
        selected.retain(|e| e.node.host == host);
        selected
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

impl Filter<Vec<Node>, Vec<Node>> for NameRegexFilter {
    fn filter(&self, mut data: Vec<Node>) -> Vec<Node> {
        data.retain(|n| {
            self.name_regexs
                .iter()
                .any(|re| re.is_match(n.remark.as_ref().unwrap()))
        });
        data
    }
}

impl Filter<SwitchData, ()> for NameRegexFilter {
    fn filter(&self, data: SwitchData) {
        let mut data = data.lock();
        *data = data
            .drain()
            .filter(|ns| {
                self.name_regexs
                    .iter()
                    .any(|re| re.is_match(ns.node.remark.as_ref().unwrap()))
            })
            .collect()
    }
}
