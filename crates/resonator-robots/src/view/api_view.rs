//! The API's view of a blip document: text with one character per element
//! (`ApiView`).

use resonator_model::conversation::blips;
use resonator_model::doc::{DocError, DocView, MutableDocument};

use super::element_serializer::{AttachmentContext, api_element_to_xml, xml_to_api_element};
use crate::api::{Element, ElementType};

/// An element of a blip with its positions (`ApiView.ElementInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementInfo {
    pub element: Element,
    /// Position in the blip's text.
    pub api_position: usize,
    /// Location in the blip's document.
    pub xml_position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    /// A run of text, in UTF-16 code units.
    Text(Vec<u16>),
    /// A child element of the body; `None` if the API has no element for
    /// it. Either way it takes one position in the text.
    Element(Option<Element>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Bit {
    content: Content,
    xml_pos: usize,
    /// Number of document items an element spans. Unused for text, whose
    /// size is its length.
    element_xml_size: usize,
}

impl Bit {
    fn text(text: Vec<u16>, xml_pos: usize) -> Bit {
        Bit {
            content: Content::Text(text),
            xml_pos,
            element_xml_size: 0,
        }
    }

    fn element(element: Option<Element>, xml_pos: usize, xml_size: usize) -> Bit {
        Bit {
            content: Content::Element(element),
            xml_pos,
            element_xml_size: xml_size,
        }
    }

    /// Size in the API text.
    fn size(&self) -> usize {
        match &self.content {
            Content::Text(text) => text.len(),
            Content::Element(_) => 1,
        }
    }

    fn xml_size(&self) -> usize {
        match &self.content {
            Content::Text(text) => text.len(),
            Content::Element(_) => self.element_xml_size,
        }
    }
}

/// The content of a blip's body flattened for the API: the text nodes
/// directly in the body as they are, and every child element of the body
/// as one character, a newline for a line element and a space for anything
/// else. Positions in that text ("API positions") map to locations of the
/// document ("XML positions") and back.
///
/// The view is built from a document and then kept in step with the edits
/// made through it ([`ApiView::insert_text`], [`ApiView::insert_element`],
/// [`ApiView::delete`]); other edits to the document need a
/// [`ApiView::reparse`].
///
/// All positions and lengths are in UTF-16 code units.
///
/// Differences from the original: after a deletion inside a run of text
/// the original keeps the run's old size for position arithmetic, and
/// when text is inserted directly before an element it records the new
/// run at a location too low by the run's length; both are corrected
/// here. Positions outside the text are errors instead of exceptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiView {
    bits: Vec<Bit>,
    attachments: AttachmentContext,
}

fn out_of_bounds(what: &str) -> DocError {
    DocError::InvalidArgument(format!("{what} is outside the blip's content"))
}

impl ApiView {
    pub fn new(view: &DocView, attachments: &AttachmentContext) -> Self {
        let mut api_view = ApiView {
            bits: Vec::new(),
            attachments: attachments.clone(),
        };
        api_view.parse(view);
        api_view
    }

    fn parse(&mut self, view: &DocView) {
        let Some(body) = blips::body(view) else {
            return;
        };
        let children = view.children(body);
        for (i, &child) in children.iter().enumerate() {
            let xml_pos = view.location(child);
            if let Some(text) = view.text(child) {
                self.bits
                    .push(Bit::text(text.encode_utf16().collect(), xml_pos));
            } else {
                let element = xml_to_api_element(view, child, &self.attachments);
                let xml_size = match children.get(i + 1) {
                    Some(&next) => view.location(next) - xml_pos,
                    // The original takes the rest of the document, less
                    // the body's end tag.
                    None => (view.size().saturating_sub(1)).saturating_sub(xml_pos),
                };
                self.bits.push(Bit::element(element, xml_pos, xml_size));
            }
        }
    }

    /// Rebuilds the view from the document.
    pub fn reparse(&mut self, view: &DocView) {
        self.bits.clear();
        self.parse(view);
    }

    /// The bit an API position falls in and the offset within it; the
    /// index is the number of bits if the position is past the end.
    fn locate(&self, mut offset: usize) -> (usize, usize) {
        let mut index = 0;
        while index < self.bits.len() && self.bits[index].size() <= offset {
            offset -= self.bits[index].size();
            index += 1;
        }
        (index, offset)
    }

    fn shift(&mut self, from: usize, delta: isize) {
        for bit in self.bits.iter_mut().skip(from) {
            bit.xml_pos = bit.xml_pos.saturating_add_signed(delta);
        }
    }

    /// Deletes the API range `start..end` from the view and the document.
    /// A start past the end of the text deletes nothing.
    pub fn delete(
        &mut self,
        doc: &mut MutableDocument<'_>,
        start: usize,
        end: usize,
    ) -> Result<(), DocError> {
        let mut len = end.saturating_sub(start);
        let (mut index, mut offset) = self.locate(start);
        if index == self.bits.len() {
            return Ok(());
        }
        let xml_start = self.bits[index].xml_pos + offset;
        let mut xml_end = xml_start;
        while len > 0 {
            let Some(bit) = self.bits.get_mut(index) else {
                return Err(out_of_bounds("the end of the range"));
            };
            match &mut bit.content {
                Content::Element(_) => {
                    let xml_size = bit.element_xml_size;
                    len -= 1;
                    self.shift(index + 1, -(xml_size as isize));
                    xml_end += xml_size;
                    self.bits.remove(index);
                }
                Content::Text(text) => {
                    let to_delete = (text.len() - offset).min(len);
                    text.drain(offset..offset + to_delete);
                    let emptied = text.is_empty();
                    self.shift(index + 1, -(to_delete as isize));
                    xml_end += to_delete;
                    len -= to_delete;
                    if offset > 0 {
                        index += 1;
                        offset = 0;
                    } else if emptied {
                        self.bits.remove(index);
                    }
                }
            }
        }
        doc.delete_range(xml_start, xml_end)
    }

    /// Inserts an element at an API position; past the end it is appended.
    pub fn insert_element(
        &mut self,
        doc: &mut MutableDocument<'_>,
        pos: usize,
        element: &Element,
    ) -> Result<(), DocError> {
        let xml = api_element_to_xml(element);
        let before_size = doc.size();
        let (index, offset) = self.locate(pos);
        if index == self.bits.len() {
            let Some(last) = self.bits.last() else {
                return Err(out_of_bounds("the blip has no content and the position"));
            };
            let at = last.xml_pos + last.xml_size();
            doc.insert_xml(at, &xml)?;
            self.bits.push(Bit::element(
                Some(element.clone()),
                at,
                doc.size() - before_size,
            ));
            return Ok(());
        }
        let at = self.bits[index].xml_pos + offset;
        doc.insert_xml(at, &xml)?;
        let xml_size = doc.size() - before_size;
        let split = match &mut self.bits[index].content {
            Content::Text(text) if offset > 0 => Some(text.split_off(offset)),
            _ => None,
        };
        match split {
            Some(left_over) => {
                self.shift(index + 1, xml_size as isize);
                self.bits
                    .insert(index + 1, Bit::element(Some(element.clone()), at, xml_size));
                self.bits
                    .insert(index + 2, Bit::text(left_over, at + xml_size));
            }
            None => {
                self.bits
                    .insert(index, Bit::element(Some(element.clone()), at, xml_size));
                self.shift(index + 1, xml_size as isize);
            }
        }
        Ok(())
    }

    /// Inserts text at an API position; past the end it is appended. Each
    /// newline in the text becomes a line element.
    pub fn insert_text(
        &mut self,
        doc: &mut MutableDocument<'_>,
        mut pos: usize,
        content: &str,
    ) -> Result<(), DocError> {
        for (i, paragraph) in content.split('\n').enumerate() {
            if i > 0 {
                self.insert_element(doc, pos, &Element::line())?;
                pos += 1;
            }
            let units: Vec<u16> = paragraph.encode_utf16().collect();
            let length = units.len();
            let (index, offset) = self.locate(pos);
            if index == self.bits.len() {
                let Some(last) = self.bits.last() else {
                    return Err(out_of_bounds("the blip has no content and the position"));
                };
                let at = last.xml_pos + last.xml_size();
                doc.insert_text(at, paragraph)?;
                self.bits.push(Bit::text(units, at));
            } else {
                let at = self.bits[index].xml_pos + offset;
                doc.insert_text(at, paragraph)?;
                match &mut self.bits[index].content {
                    Content::Text(text) => {
                        text.splice(offset..offset, units);
                    }
                    Content::Element(_) => {
                        self.bits.insert(index, Bit::text(units, at));
                    }
                }
                self.shift(index + 1, length as isize);
            }
            pos += length;
        }
        Ok(())
    }

    /// The blip's text as UTF-16 code units.
    pub fn api_contents_utf16(&self) -> Vec<u16> {
        let mut result = Vec::new();
        for bit in &self.bits {
            match &bit.content {
                Content::Text(text) => result.extend_from_slice(text),
                Content::Element(Some(element)) if element.element_type == ElementType::Line => {
                    result.push('\n' as u16);
                }
                Content::Element(_) => result.push(' ' as u16),
            }
        }
        result
    }

    /// The blip's text (`apiContents`).
    pub fn api_contents(&self) -> String {
        String::from_utf16_lossy(&self.api_contents_utf16())
    }

    /// The length of the blip's text in UTF-16 code units.
    pub fn len(&self) -> usize {
        self.bits.iter().map(Bit::size).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// The known elements with their positions (`getElements`).
    pub fn elements(&self) -> Vec<ElementInfo> {
        let mut result = Vec::new();
        let mut index = 0;
        for bit in &self.bits {
            if let Content::Element(Some(element)) = &bit.content {
                result.push(ElementInfo {
                    element: element.clone(),
                    api_position: index,
                    xml_position: bit.xml_pos,
                });
            }
            index += bit.size();
        }
        result
    }

    /// The API position of a document location
    /// (`transformToTextOffset`): a location inside a known element maps
    /// to the element's position, one past the body's content to the
    /// length of the text.
    pub fn transform_to_text_offset(&self, xml_offset: usize) -> usize {
        let mut index = 0;
        let mut text_offset = 0;
        while index < self.bits.len()
            && self.bits[index].xml_pos + self.bits[index].xml_size() < xml_offset + 1
        {
            text_offset += self.bits[index].size();
            index += 1;
        }
        let Some(bit) = self.bits.get(index) else {
            return text_offset;
        };
        match bit.content {
            Content::Element(Some(_)) => text_offset,
            // As in the original, a location inside an element the API
            // does not know counts like one inside text.
            Content::Element(None) | Content::Text(_) => {
                text_offset + xml_offset.saturating_sub(bit.xml_pos)
            }
        }
    }

    /// The document location of an API position (`transformToXmlOffset`).
    /// Positions past the end continue after the last bit; `None` if the
    /// blip has no content at all.
    pub fn transform_to_xml_offset(&self, api_index: usize) -> Option<usize> {
        let (index, offset) = self.locate(api_index);
        match self.bits.get(index) {
            Some(bit) => Some(bit.xml_pos + offset),
            None => {
                let last = self.bits.last()?;
                Some(last.xml_pos + last.xml_size() + offset)
            }
        }
    }

    /// The API position of the first element like `element`
    /// (`locateElement`): of the same type and, for a gadget, with the
    /// same URL; for a label, with the same `for` property; otherwise
    /// with the same properties.
    pub fn locate_element(&self, element: &Element) -> Option<usize> {
        let property_match = |a: &Element, b: &Element, name: &str| {
            a.property(name).is_some() && a.property(name) == b.property(name)
        };
        let mut index = 0;
        for bit in &self.bits {
            if let Content::Element(Some(candidate)) = &bit.content
                && candidate.element_type == element.element_type
            {
                let matches = match element.element_type {
                    ElementType::Gadget => property_match(candidate, element, Element::URL),
                    ElementType::Label => property_match(candidate, element, "for"),
                    _ => candidate.properties == element.properties,
                };
                if matches {
                    return Some(index);
                }
            }
            index += bit.size();
        }
        None
    }
}
