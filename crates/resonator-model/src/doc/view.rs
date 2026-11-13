//! A read-only element tree over a document.

use crate::docop::{Attributes, BootstrapDocument, DocOpComponent};

/// A node of a [`DocView`]. Only meaningful for the view it came from: any
/// change to the document invalidates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeData {
    Element { tag: String, attributes: Attributes },
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    data: NodeData,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    index_in_parent: usize,
    /// Location just before the node (for the root: 0).
    location: usize,
    /// Number of document items the node spans, tags included.
    size: usize,
}

/// A place between two items of a document (`Point`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// Inside a text node, `offset` UTF-16 code units from its start.
    InText { node: NodeId, offset: usize },
    /// Inside `container`, before `node_after`, or at the end of the
    /// container if `None`.
    InElement {
        container: NodeId,
        node_after: Option<NodeId>,
    },
}

/// The element tree of a document at one moment, with the location of every
/// node. Locations count items as document operations do: one per UTF-16
/// code unit of text, one per element start and one per element end.
///
/// The root node stands for the document itself (the original's implicit
/// `doc` element); it has no tags, so the inside of the root spans
/// locations `0..=size`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocView {
    nodes: Vec<Node>,
}

/// The tag name the original gives the implicit root element.
pub const ROOT_TAG: &str = "doc";

impl DocView {
    /// Builds the tree of `doc`.
    pub fn new(doc: &BootstrapDocument) -> DocView {
        let mut nodes = vec![Node {
            data: NodeData::Element {
                tag: ROOT_TAG.to_owned(),
                attributes: Attributes::new(),
            },
            parent: None,
            children: Vec::new(),
            index_in_parent: 0,
            location: 0,
            size: 0,
        }];
        let mut current = 0usize;
        let mut location = 0usize;
        let add = |nodes: &mut Vec<Node>, parent: usize, data: NodeData, location: usize| {
            let id = nodes.len();
            let index_in_parent = nodes[parent].children.len();
            nodes[parent].children.push(NodeId(id as u32));
            nodes.push(Node {
                data,
                parent: Some(NodeId(parent as u32)),
                children: Vec::new(),
                index_in_parent,
                location,
                size: 0,
            });
            id
        };
        for component in doc.as_operation().iter() {
            match component {
                DocOpComponent::Characters(chars) => {
                    let length = chars.encode_utf16().count();
                    // Text split by annotation boundaries is one node.
                    let last = nodes[current].children.last().map(|id| id.0 as usize);
                    match last {
                        Some(last) if matches!(nodes[last].data, NodeData::Text(_)) => {
                            if let NodeData::Text(text) = &mut nodes[last].data {
                                text.push_str(chars);
                            }
                            nodes[last].size += length;
                        }
                        _ => {
                            let id =
                                add(&mut nodes, current, NodeData::Text(chars.clone()), location);
                            nodes[id].size = length;
                        }
                    }
                    location += length;
                }
                DocOpComponent::ElementStart { tag, attributes } => {
                    current = add(
                        &mut nodes,
                        current,
                        NodeData::Element {
                            tag: tag.clone(),
                            attributes: attributes.clone(),
                        },
                        location,
                    );
                    location += 1;
                }
                DocOpComponent::ElementEnd => {
                    location += 1;
                    nodes[current].size = location - nodes[current].location;
                    // A document is balanced, so there is always a parent.
                    current = nodes[current].parent.map_or(0, |id| id.0 as usize);
                }
                _ => {}
            }
        }
        nodes[0].size = location;
        DocView { nodes }
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0 as usize)
    }

    /// The node standing for the whole document.
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Number of items in the document.
    pub fn size(&self) -> usize {
        self.nodes[0].size
    }

    pub fn is_element(&self, node: NodeId) -> bool {
        matches!(
            self.node(node),
            Some(Node {
                data: NodeData::Element { .. },
                ..
            })
        )
    }

    pub fn is_text(&self, node: NodeId) -> bool {
        self.text(node).is_some()
    }

    /// The tag name of an element; `None` for text nodes.
    pub fn tag_name(&self, node: NodeId) -> Option<&str> {
        match &self.node(node)?.data {
            NodeData::Element { tag, .. } => Some(tag),
            NodeData::Text(_) => None,
        }
    }

    pub fn has_tag(&self, node: NodeId, tag: &str) -> bool {
        self.tag_name(node) == Some(tag)
    }

    pub fn attributes(&self, node: NodeId) -> Option<&Attributes> {
        match &self.node(node)?.data {
            NodeData::Element { attributes, .. } => Some(attributes),
            NodeData::Text(_) => None,
        }
    }

    pub fn attribute(&self, node: NodeId, name: &str) -> Option<&str> {
        self.attributes(node)?.get(name)
    }

    /// The data of a text node.
    pub fn text(&self, node: NodeId) -> Option<&str> {
        match &self.node(node)?.data {
            NodeData::Text(text) => Some(text),
            NodeData::Element { .. } => None,
        }
    }

    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.node(node)?.parent
    }

    pub fn children(&self, node: NodeId) -> &[NodeId] {
        self.node(node).map_or(&[], |n| &n.children)
    }

    /// The element children of a node.
    pub fn child_elements(&self, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(node)
            .iter()
            .copied()
            .filter(|&child| self.is_element(child))
    }

    pub fn first_child(&self, node: NodeId) -> Option<NodeId> {
        self.children(node).first().copied()
    }

    pub fn last_child(&self, node: NodeId) -> Option<NodeId> {
        self.children(node).last().copied()
    }

    pub fn next_sibling(&self, node: NodeId) -> Option<NodeId> {
        let n = self.node(node)?;
        self.children(n.parent?).get(n.index_in_parent + 1).copied()
    }

    pub fn previous_sibling(&self, node: NodeId) -> Option<NodeId> {
        let n = self.node(node)?;
        let index = n.index_in_parent.checked_sub(1)?;
        self.children(n.parent?).get(index).copied()
    }

    /// `DocHelper.getFirstChildElement`.
    pub fn first_child_element(&self, node: NodeId) -> Option<NodeId> {
        self.child_elements(node).next()
    }

    /// `DocHelper.getNextSiblingElement`.
    pub fn next_sibling_element(&self, node: NodeId) -> Option<NodeId> {
        let mut next = self.next_sibling(node);
        while let Some(n) = next {
            if self.is_element(n) {
                return Some(n);
            }
            next = self.next_sibling(n);
        }
        None
    }

    /// The node itself if an element, else its parent
    /// (`Point.enclosingElement`).
    pub fn enclosing_element(&self, node: NodeId) -> NodeId {
        if self.is_element(node) {
            node
        } else {
            self.parent(node).unwrap_or(self.root())
        }
    }

    /// Location just before a node (`Locator.before`); 0 for the root.
    pub fn location(&self, node: NodeId) -> usize {
        self.node(node).map_or(0, |n| n.location)
    }

    /// Location just after a node (`Locator.after`).
    pub fn location_after(&self, node: NodeId) -> usize {
        self.node(node).map_or(0, |n| n.location + n.size)
    }

    /// Location of the start of an element's content (`Locator.start`).
    pub fn content_start(&self, element: NodeId) -> usize {
        if element == self.root() {
            0
        } else {
            self.location(element) + 1
        }
    }

    /// Location of the end of an element's content (`Locator.end`).
    pub fn content_end(&self, element: NodeId) -> usize {
        if element == self.root() {
            self.size()
        } else {
            self.location_after(element).saturating_sub(1)
        }
    }

    /// Number of items a node spans, including its tags.
    pub fn item_size(&self, node: NodeId) -> usize {
        self.node(node).map_or(0, |n| n.size)
    }

    /// The location of a point, or `None` if the point is not valid for
    /// this view.
    pub fn point_location(&self, point: Point) -> Option<usize> {
        match point {
            Point::InText { node, offset } => {
                let n = self.node(node)?;
                (matches!(n.data, NodeData::Text(_)) && offset <= n.size)
                    .then_some(n.location + offset)
            }
            Point::InElement {
                container,
                node_after,
            } => {
                if !self.is_element(container) {
                    return None;
                }
                match node_after {
                    None => Some(self.content_end(container)),
                    Some(after) => {
                        (self.parent(after) == Some(container)).then(|| self.location(after))
                    }
                }
            }
        }
    }

    /// The point at a location: inside a text node if the location is
    /// strictly inside one, otherwise between nodes. `None` if the location
    /// is beyond the document.
    pub fn locate(&self, location: usize) -> Option<Point> {
        if location > self.size() {
            return None;
        }
        let mut container = self.root();
        loop {
            let mut descended = false;
            for &child in self.children(container) {
                let start = self.location(child);
                let end = self.location_after(child);
                if location <= start {
                    return Some(Point::InElement {
                        container,
                        node_after: Some(child),
                    });
                }
                if location < end {
                    if self.is_text(child) {
                        return Some(Point::InText {
                            node: child,
                            offset: location - start,
                        });
                    }
                    container = child;
                    descended = true;
                    break;
                }
            }
            if !descended {
                return Some(Point::InElement {
                    container,
                    node_after: None,
                });
            }
        }
    }

    /// The element whose content directly holds a location.
    pub fn element_at(&self, location: usize) -> Option<NodeId> {
        Some(match self.locate(location)? {
            Point::InText { node, .. } => self.enclosing_element(node),
            Point::InElement { container, .. } => container,
        })
    }

    /// The node starting exactly at a location, if any.
    pub fn node_starting_at(&self, location: usize) -> Option<NodeId> {
        match self.locate(location)? {
            Point::InElement { node_after, .. } => {
                node_after.filter(|&n| self.location(n) == location)
            }
            Point::InText { .. } => None,
        }
    }

    /// The element starting exactly at a location, if any.
    pub fn element_starting_at(&self, location: usize) -> Option<NodeId> {
        self.node_starting_at(location)
            .filter(|&n| self.is_element(n))
    }

    /// All nodes below `root` in document order (`DocIterate.deep`).
    pub fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut stack: Vec<NodeId> = self.children(root).iter().rev().copied().collect();
        while let Some(node) = stack.pop() {
            result.push(node);
            stack.extend(self.children(node).iter().rev().copied());
        }
        result
    }

    /// All elements below `root` in document order
    /// (`DocIterate.deepElements`).
    pub fn descendant_elements(&self, root: NodeId) -> Vec<NodeId> {
        let mut result = self.descendants(root);
        result.retain(|&node| self.is_element(node));
        result
    }

    /// The first element below `root`, in document order, with a tag name
    /// (`DocHelper.getElementWithTagName`).
    pub fn element_with_tag_name(&self, tag: &str, root: NodeId) -> Option<NodeId> {
        self.descendants(root)
            .into_iter()
            .find(|&node| self.has_tag(node, tag))
    }

    /// The text at locations `start..end`, skipping element tags
    /// (`DocHelper.getText`). Out-of-range bounds are clamped.
    pub fn text_between(&self, start: usize, end: usize) -> String {
        let mut units: Vec<u16> = Vec::new();
        for node in self.descendants(self.root()) {
            let Some(text) = self.text(node) else {
                continue;
            };
            let location = self.location(node);
            if location >= end {
                break;
            }
            units.extend(
                text.encode_utf16()
                    .enumerate()
                    .filter(|(i, _)| (start..end).contains(&(location + i)))
                    .map(|(_, unit)| unit),
            );
        }
        String::from_utf16_lossy(&units)
    }

    /// All text below a node (`DocHelper.getText` of an element).
    pub fn text_content(&self, node: NodeId) -> String {
        if let Some(text) = self.text(node) {
            return text.to_owned();
        }
        self.descendants(node)
            .into_iter()
            .filter_map(|n| self.text(n))
            .collect()
    }
}
