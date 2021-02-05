use std::path::Path;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub host: Option<String>,
    pub path: Option<String>,
    pub tls: Option<String>,
    #[serde(rename = "verify_cert")]
    pub verify_cert: Option<bool>,
    pub add: Option<String>,
    pub port: Option<u16>,
    pub aid: Option<u16>,
    pub net: Option<String>,
    pub header_type: Option<String>,
    pub localserver: Option<String>,
    pub v: Option<String>,
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    pub ps: Option<String>,
    pub remark: Option<String>,
    pub id: Option<String>,
    pub class: Option<u16>,
}

pub async fn load_subscription_nodes_from_file<T: AsRef<Path>>(path: T) -> Result<Vec<Node>> {
    let buf = tokio::fs::read(path).await?;
    parse_subscription_nodes(buf)
}

pub fn parse_subscription_nodes<T: AsRef<[u8]>>(buf: T) -> Result<Vec<Node>> {
    let buf = buf.as_ref();
    base64::decode(buf)?
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(parse_node)
        .collect()
}

pub fn parse_node<T: AsRef<[u8]>>(buf: T) -> Result<Node> {
    const PREFIX: &str = "vmess://";
    let buf = buf.as_ref();
    if !buf.starts_with(PREFIX.as_bytes()) {
        return Err(anyhow!(
            "only supported prefix: {:?}, cur: {:?}",
            PREFIX.as_bytes(),
            &buf[..PREFIX.len()]
        ));
    }
    let buf = base64::decode(&buf[PREFIX.len()..])?;
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod node_tests {
    use super::*;

    #[test]
    fn parse() -> Result<()> {
        let node_bytes = r#"vmess://eyJob3N0IjoiYXUwMS5hd3Muamlua2VsYS5pY3UiLCJwYXRoIjoiL2hscyIsInRscyI6IiIsInZlcmlmeV9jZXJ0Ijp0cnVlLCJhZGQiOiJhaC5jbmN1LmxheTE2OC5uZXQiLCJwb3J0Ijo2MTAzNSwiYWlkIjoyLCJuZXQiOiJ3cyIsImhlYWRlclR5cGUiOiJub25lIiwibG9jYWxzZXJ2ZXIiOiJhdTAxLmF3cy5qaW5rZWxhLmljdSIsInYiOiIyIiwidHlwZSI6InZtZXNzIiwicHMiOiLlronlvr3ihpLmvrPmtLIwMSDlhrfpl6ggfCAxLjV4IiwicmVtYXJrIjoi5a6J5b694oaS5r6z5rSyMDEg5Ya36ZeoIHwgMS41eCIsImlkIjoiNTVmYjA0NTctZDg3NC0zMmMzLTg5YTItNjc5ZmVkNmVhYmYxIiwiY2xhc3MiOjF9"#;
        let node = parse_node(node_bytes)?;
        let expected: Node = serde_json::from_str(
            r#"{
            "host": "au01.aws.jinkela.icu",
            "path": "/hls",
            "tls": "",
            "verify_cert": true,
            "add": "ah.cncu.lay168.net",
            "port": 61035,
            "aid": 2,
            "net": "ws",
            "headerType": "none",
            "localserver": "au01.aws.jinkela.icu",
            "v": "2",
            "type": "vmess",
            "ps": "安徽→澳洲01 冷门 | 1.5x",
            "remark": "安徽→澳洲01 冷门 | 1.5x",
            "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
            "class": 1
        }"#,
        )?;
        assert_eq!(node, expected);
        Ok(())
    }

    // #[test]
    #[tokio::test]
    async fn load_subscription_nodes_from_file_test() -> Result<()> {
        let path = "/home/navyd/Workspaces/projects/tasks/src/test/resources/V2RayN_1611312530.txt";
        let nodes = load_subscription_nodes_from_file(path).await?;
        assert_eq!(nodes.len(), 147);
        Ok(())
    }

    #[test]
    fn parse_subscription_nodes_test() -> Result<()> {
        // 双重encode
        let buf = r#"dm1lc3M6Ly9leUpvYjNOMElqb2ljblV3Tmk1cWFDNXVZbk5rTG5Weklpd2ljR0YwYUNJNklpOW9iSE1pTENKMGJITWlPaUlpTENKMlpYSnBabmxmWTJWeWRDSTZkSEoxWlN3aVlXUmtJam9pWVdndVkyNWpkUzVzWVhreE5qZ3VibVYwSWl3aWNHOXlkQ0k2TmpFd05EUXNJbUZwWkNJNk1pd2libVYwSWpvaWQzTWlMQ0pvWldGa1pYSlVlWEJsSWpvaWJtOXVaU0lzSW14dlkyRnNjMlZ5ZG1WeUlqb2ljblV3Tmk1cWFDNXVZbk5rTG5Weklpd2lkaUk2SWpJaUxDSjBlWEJsSWpvaWRtMWxjM01pTENKd2N5STZJdVd1aWVXK3ZlS0drdVMvaE9lOWwrYVdyekEySUh3Z01TNHplQ0lzSW5KbGJXRnlheUk2SXVXdWllVyt2ZUtHa3VTL2hPZTlsK2FXcnpBMklId2dNUzR6ZUNJc0ltbGtJam9pTlRWbVlqQTBOVGN0WkRnM05DMHpNbU16TFRnNVlUSXROamM1Wm1Wa05tVmhZbVl4SWl3aVkyeGhjM01pT2pGOQp2bWVzczovL2V5Sm9iM04wSWpvaWNuVXdOUzVxYUM1dVluTmtMblZ6SWl3aWNHRjBhQ0k2SWk5b2JITWlMQ0owYkhNaU9pSWlMQ0oyWlhKcFpubGZZMlZ5ZENJNmRISjFaU3dpWVdSa0lqb2lZV2d1WTI1amRTNXNZWGt4TmpndWJtVjBJaXdpY0c5eWRDSTZOakV3TkRJc0ltRnBaQ0k2TWl3aWJtVjBJam9pZDNNaUxDSm9aV0ZrWlhKVWVYQmxJam9pYm05dVpTSXNJbXh2WTJGc2MyVnlkbVZ5SWpvaWNuVXdOUzVxYUM1dVluTmtMblZ6SWl3aWRpSTZJaklpTENKMGVYQmxJam9pZG0xbGMzTWlMQ0p3Y3lJNkl1V3VpZVcrdmVLR2t1Uy9oT2U5bCthV3J6QTFJSHdnTVM0emVGeDBJaXdpY21WdFlYSnJJam9pNWE2SjViNjk0b2FTNUwrRTU3Mlg1cGF2TURVZ2ZDQXhMak40WEhRaUxDSnBaQ0k2SWpVMVptSXdORFUzTFdRNE56UXRNekpqTXkwNE9XRXlMVFkzT1dabFpEWmxZV0ptTVNJc0ltTnNZWE56SWpveGZRPT0="#;
        let nodes = parse_subscription_nodes(buf)?;
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].host, Some("ru06.jh.nbsd.us".to_owned()));
        assert_eq!(nodes[1].host, Some("ru05.jh.nbsd.us".to_owned()));
        Ok(())
    }

    #[test]
    fn serde_test() {
        let content = r#"{
            "host": "jp02.li.jinkela.icu",
            "path": "/hls",
            "tls": "",
            "verify_cert": true,
            "add": "sh01.unicom.lay168.net",
            "port": 61017,
            "aid": 2,
            "net": "ws",
            "headerType": "none",
            "localserver": "jp02.li.jinkela.icu",
            "v": "2",
            "type": "none",
            "ps": "上海01→日本02 | 1.5x NF",
            "remark": "上海01→日本02 | 1.5x NF",
            "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
            "class": 1
        }"#;
        let node: Node = serde_json::from_str(content).unwrap();
        assert_eq!(node.add, Some("sh01.unicom.lay168.net".to_string()));
        assert_eq!(node.node_type, Some("none".to_string()));
        assert_eq!(node.verify_cert, Some(true));
        assert_eq!(node.header_type, Some("none".to_string()));
    }
}
