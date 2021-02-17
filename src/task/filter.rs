use std::{collections::BinaryHeap, sync::Arc};

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
        log::trace!("{} of nodes left", selected.len());
        if selected.is_empty() {
            log::warn!("All nodes are filtered");
        }
        selected
    }
}

pub struct NameRegexFilter {
    name_regex: Regex,
}

impl NameRegexFilter {
    pub fn new(name_regex: &str) -> Self {
        if name_regex.is_empty() {
            panic!("Empty name regexs");
        }
        Self {
            name_regex: Regex::new(name_regex)
                .unwrap_or_else(|e| panic!("regex `{}` error: {}", name_regex, e)),
        }
    }
}

impl Filter<Vec<Node>, Vec<Node>> for NameRegexFilter {
    fn filter(&self, mut data: Vec<Node>) -> Vec<Node> {
        log::trace!("filtering data: {} by name regex: {}", data.len(), self.name_regex);
        data.retain(|n| self.name_regex.is_match(n.remark.as_ref().unwrap()));
        log::trace!("{} of nodes left", data.len());
        if data.is_empty() {
            log::warn!("All nodes are filtered");
        }
        data
    }
}

impl Filter<BinaryHeap<SwitchNodeStat>, BinaryHeap<SwitchNodeStat>> for NameRegexFilter {
    fn filter(&self, mut data: BinaryHeap<SwitchNodeStat>) -> BinaryHeap<SwitchNodeStat> {
        log::trace!("filtering data: {} by name regex: {}", data.len(), self.name_regex);
        let data = data
            .drain()
            .filter(|ns| self.name_regex.is_match(ns.node.remark.as_ref().unwrap()))
            .collect::<BinaryHeap<SwitchNodeStat>>();
        log::trace!("{} of nodes left", data.len());
        if data.is_empty() {
            log::warn!("All nodes are filtered");
        }
        data
    }
}
