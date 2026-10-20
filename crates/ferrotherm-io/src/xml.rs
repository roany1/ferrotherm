// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Minimal XML DOM on top of quick-xml (namespace prefixes stripped).

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Clone, Default)]
pub struct XNode {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<XNode>,
    pub text: String,
}

impl XNode {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        self.attr(k)?.trim().parse().ok()
    }
    pub fn child(&self, name: &str) -> Option<&XNode> {
        self.children.iter().find(|c| c.name == name)
    }
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a XNode> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    /// Depth-first search for every descendant with this name.
    pub fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a XNode>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            }
            c.descendants(name, out);
        }
    }
    pub fn find(&self, name: &str) -> Option<&XNode> {
        for c in &self.children {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = c.find(name) {
                return Some(f);
            }
        }
        None
    }
}

fn local(name: &str) -> String {
    let s = name;
    match s.rsplit_once(':') {
        Some((_, l)) => l.to_string(),
        None => s.to_string(),
    }
}

fn node_from(e: &quick_xml::events::BytesStart) -> XNode {
    let mut n = XNode {
        name: local(e.name().as_ref()),
        ..Default::default()
    };
    for a in e.attributes().flatten() {
        let v = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|c| c.to_string())
            .unwrap_or_else(|_| a.value.to_string());
        n.attrs.push((local(a.key.as_ref()), v));
    }
    n
}

pub fn parse(text: &str) -> Result<XNode, String> {
    let mut r = Reader::from_str(text);
    r.config_mut().trim_text(true);
    let mut stack: Vec<XNode> = vec![XNode {
        name: "#document".into(),
        ..Default::default()
    }];
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => stack.push(node_from(&e)),
            Ok(Event::Empty(e)) => {
                let n = node_from(&e);
                stack.last_mut().unwrap().children.push(n);
            }
            Ok(Event::End(_)) => {
                let n = stack.pop().ok_or("unbalanced XML")?;
                stack.last_mut().ok_or("unbalanced XML")?.children.push(n);
            }
            Ok(Event::Text(t)) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML error at byte {}: {e}", r.buffer_position())),
            _ => {}
        }
    }
    let mut doc = stack.pop().ok_or("empty XML")?;
    if !stack.is_empty() {
        return Err("unterminated XML elements".into());
    }
    doc.children.pop().ok_or_else(|| "empty XML".into())
}
