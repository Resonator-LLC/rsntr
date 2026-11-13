//! The data model of the Robots API: wavelets, blips, threads, elements,
//! annotations, search results and profiles, with the JSON shapes Gson
//! gives the original classes.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::lenient;

/// The kind of an [`Element`] (`ElementType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ElementType {
    Input,
    Password,
    Check,
    Label,
    Button,
    RadioButton,
    RadioButtonGroup,
    Textarea,
    InlineBlip,
    Gadget,
    Installer,
    Image,
    Line,
    Attachment,
}

impl ElementType {
    pub const ALL: [ElementType; 14] = [
        ElementType::Input,
        ElementType::Password,
        ElementType::Check,
        ElementType::Label,
        ElementType::Button,
        ElementType::RadioButton,
        ElementType::RadioButtonGroup,
        ElementType::Textarea,
        ElementType::InlineBlip,
        ElementType::Gadget,
        ElementType::Installer,
        ElementType::Image,
        ElementType::Line,
        ElementType::Attachment,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ElementType::Input => "INPUT",
            ElementType::Password => "PASSWORD",
            ElementType::Check => "CHECK",
            ElementType::Label => "LABEL",
            ElementType::Button => "BUTTON",
            ElementType::RadioButton => "RADIO_BUTTON",
            ElementType::RadioButtonGroup => "RADIO_BUTTON_GROUP",
            ElementType::Textarea => "TEXTAREA",
            ElementType::InlineBlip => "INLINE_BLIP",
            ElementType::Gadget => "GADGET",
            ElementType::Installer => "INSTALLER",
            ElementType::Image => "IMAGE",
            ElementType::Line => "LINE",
            ElementType::Attachment => "ATTACHMENT",
        }
    }

    /// `ElementType.valueOfIgnoreCase`.
    pub fn from_name_ignore_case(name: &str) -> Option<ElementType> {
        let upper = name.to_uppercase();
        ElementType::ALL.into_iter().find(|t| t.as_str() == upper)
    }

    /// Whether elements of this type are form elements
    /// (`FormElement.getFormElementTypes`).
    pub fn is_form_element(self) -> bool {
        matches!(
            self,
            ElementType::Button
                | ElementType::Check
                | ElementType::Input
                | ElementType::Password
                | ElementType::Label
                | ElementType::RadioButton
                | ElementType::RadioButtonGroup
                | ElementType::Textarea
        )
    }
}

impl fmt::Display for ElementType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ElementType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        ElementType::from_name_ignore_case(s).ok_or_else(|| format!("unknown element type {s}"))
    }
}

impl Serialize for ElementType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ElementType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        name.parse().map_err(D::Error::custom)
    }
}

/// Something in a blip that is not text: a line break, a form element, a
/// gadget, an image, an inline reply anchor, an attachment (`Element` and
/// its subclasses, which differ only in their property names).
///
/// A property can be present with a null value, as in the original's map.
/// The data of an attachment is the base64 property `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    #[serde(rename = "type")]
    pub element_type: ElementType,
    #[serde(default, deserialize_with = "lenient::opt_string_map")]
    pub properties: BTreeMap<String, Option<String>>,
}

impl Element {
    // Property names of the original's subclasses.
    pub const NAME: &'static str = "name";
    pub const VALUE: &'static str = "value";
    pub const DEFAULT_VALUE: &'static str = "defaultValue";
    pub const URL: &'static str = "url";
    pub const AUTHOR: &'static str = "author";
    pub const IFRAME: &'static str = "ifr";
    pub const CAPTION: &'static str = "caption";
    pub const ATTACHMENT_ID: &'static str = "attachmentId";
    pub const ATTACHMENT_URL: &'static str = "attachmentUrl";
    pub const MIME_TYPE: &'static str = "mimeType";
    pub const DATA: &'static str = "data";
    pub const LINE_TYPE: &'static str = "lineType";
    pub const INDENT: &'static str = "indent";
    pub const ALIGNMENT: &'static str = "alignment";
    pub const DIRECTION: &'static str = "direction";

    pub fn new(element_type: ElementType) -> Self {
        Element {
            element_type,
            properties: BTreeMap::new(),
        }
    }

    pub fn with_properties<K: Into<String>, V: Into<String>>(
        element_type: ElementType,
        properties: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Element {
            element_type,
            properties: properties
                .into_iter()
                .map(|(k, v)| (k.into(), Some(v.into())))
                .collect(),
        }
    }

    /// A line break (`Line`).
    pub fn line() -> Self {
        Element::new(ElementType::Line)
    }

    /// A gadget with a specification URL (`Gadget(String)`).
    pub fn gadget(url: &str) -> Self {
        Element::with_properties(ElementType::Gadget, [(Self::URL, url)])
    }

    /// A form element (`FormElement(type, name, defaultValue, value)`).
    pub fn form(element_type: ElementType, name: &str, default_value: &str, value: &str) -> Self {
        Element::with_properties(
            element_type,
            [
                (Self::NAME, name),
                (Self::DEFAULT_VALUE, default_value),
                (Self::VALUE, value),
            ],
        )
    }

    /// An external image (`Image(url, width, height, caption)`).
    pub fn image(url: &str, width: i32, height: i32, caption: &str) -> Self {
        Element::with_properties(
            ElementType::Image,
            [
                (Self::URL, url.to_owned()),
                ("width", width.to_string()),
                ("height", height.to_string()),
                (Self::CAPTION, caption.to_owned()),
            ],
        )
    }

    /// The value of a property; `None` if absent or null (`getProperty`).
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties.get(name)?.as_deref()
    }

    pub fn has_property(&self, name: &str) -> bool {
        self.properties.contains_key(name)
    }

    pub fn set_property(&mut self, name: &str, value: impl Into<String>) {
        self.properties.insert(name.to_owned(), Some(value.into()));
    }

    pub(crate) fn set_opt_property(&mut self, name: &str, value: Option<&str>) {
        self.properties
            .insert(name.to_owned(), value.map(str::to_owned));
    }

    pub fn is_form_element(&self) -> bool {
        self.element_type.is_form_element()
    }

    pub fn is_gadget(&self) -> bool {
        self.element_type == ElementType::Gadget
    }

    pub fn is_inline_blip(&self) -> bool {
        self.element_type == ElementType::InlineBlip
    }

    pub fn is_image(&self) -> bool {
        self.element_type == ElementType::Image
    }

    pub fn is_attachment(&self) -> bool {
        self.element_type == ElementType::Attachment
    }

    /// The character that stands for the element in a blip's text
    /// (`getText`).
    pub fn text(&self) -> char {
        if self.element_type == ElementType::Line {
            '\n'
        } else {
            ' '
        }
    }

    /// The element as protocol 0.2 writes it: an attachment becomes an
    /// image without the attachment-only properties
    /// (`ElementGsonAdaptorV2.serialize`).
    pub fn to_v2(&self) -> Element {
        if self.element_type != ElementType::Attachment {
            return self.clone();
        }
        Element {
            element_type: ElementType::Image,
            properties: self
                .properties
                .iter()
                .filter(|(key, _)| {
                    ![Self::MIME_TYPE, Self::DATA, Self::ATTACHMENT_URL].contains(&key.as_str())
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// The element protocol 0.2 means: an image without a URL is an
    /// attachment (`ElementGsonAdaptorV2.deserialize`).
    pub fn from_v2(mut self) -> Element {
        if self.element_type == ElementType::Image && !self.has_property(Self::URL) {
            self.element_type = ElementType::Attachment;
        }
        self
    }
}

/// A half-open range of a blip's text, in UTF-16 code units (`Range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Range {
    #[serde(default = "minus_one_i32", deserialize_with = "lenient::int32")]
    pub start: i32,
    #[serde(default = "minus_one_i32", deserialize_with = "lenient::int32")]
    pub end: i32,
}

impl Range {
    pub fn new(start: i32, end: i32) -> Self {
        Range { start, end }
    }
}

/// A key-value pair over a range of a blip's text (`Annotation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub value: Option<String>,
    #[serde(default = "Annotation::no_range")]
    pub range: Range,
}

impl Annotation {
    pub const BACKGROUND_COLOR: &'static str = "style/backgroundColor";
    pub const COLOR: &'static str = "style/color";
    pub const FONT_FAMILY: &'static str = "style/fontFamily";
    pub const FONT_SIZE: &'static str = "style/fontSize";
    pub const FONT_STYLE: &'static str = "style/fontStyle";
    pub const FONT_WEIGHT: &'static str = "style/fontWeight";
    pub const TEXT_DECORATION: &'static str = "style/textDecoration";
    pub const VERTICAL_ALIGN: &'static str = "style/verticalAlign";
    pub const LINK: &'static str = "link/manual";
    pub const WAVE_LINK: &'static str = "link/wave";

    pub fn new(name: &str, value: &str, start: i32, end: i32) -> Self {
        Annotation {
            name: Some(name.to_owned()),
            value: Some(value.to_owned()),
            range: Range::new(start, end),
        }
    }

    fn no_range() -> Range {
        Range::new(-1, -1)
    }
}

/// A thread of blips (`BlipThread`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlipThread {
    /// The empty string for the root thread.
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub id: Option<String>,
    /// Where the thread is anchored in its parent blip's text; -1 if it is
    /// not inline.
    #[serde(default = "minus_one_i32", deserialize_with = "lenient::int32")]
    pub location: i32,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub blip_ids: Vec<String>,
}

impl BlipThread {
    pub fn new(id: &str, location: i32, blip_ids: Vec<String>) -> Self {
        BlipThread {
            id: Some(id.to_owned()),
            location,
            blip_ids,
        }
    }
}

/// A blip as the API presents it (`BlipData`): its text with one character
/// per element, the elements by position, and annotations over the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlipData {
    #[serde(default, deserialize_with = "lenient::null_default")]
    pub annotations: Vec<Annotation>,
    #[serde(default, deserialize_with = "lenient::null_default")]
    pub elements: BTreeMap<i32, Element>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub blip_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub child_blip_ids: Vec<String>,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub contributors: Vec<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub creator: Option<String>,
    #[serde(default = "newline", deserialize_with = "content")]
    pub content: String,
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub last_modified_time: i64,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub parent_blip_id: Option<String>,
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub version: i64,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub wave_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub wavelet_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub reply_thread_ids: Vec<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub thread_id: Option<String>,
}

impl Default for BlipData {
    /// `BlipData()`.
    fn default() -> Self {
        BlipData {
            annotations: Vec::new(),
            elements: BTreeMap::new(),
            blip_id: None,
            child_blip_ids: Vec::new(),
            contributors: Vec::new(),
            creator: None,
            content: newline(),
            last_modified_time: -1,
            parent_blip_id: None,
            version: -1,
            wave_id: None,
            wavelet_id: None,
            reply_thread_ids: Vec::new(),
            thread_id: None,
        }
    }
}

impl BlipData {
    /// The blip data a client sends to create a blip
    /// (`BlipData(waveId, waveletId, blipId, initialContent)`): the
    /// content gets a leading newline if it has none.
    pub fn new(wave_id: &str, wavelet_id: &str, blip_id: &str, initial_content: &str) -> Self {
        let content = if initial_content.starts_with('\n') {
            initial_content.to_owned()
        } else {
            format!("\n{initial_content}")
        };
        BlipData {
            blip_id: Some(blip_id.to_owned()),
            wave_id: Some(wave_id.to_owned()),
            wavelet_id: Some(wavelet_id.to_owned()),
            content,
            // The original's constructor leaves these at Java's zero.
            last_modified_time: 0,
            version: 0,
            ..BlipData::default()
        }
    }
}

/// A wavelet as the API presents it (`WaveletData`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveletData {
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub creation_time: i64,
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub last_modified_time: i64,
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub version: i64,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub participants: Vec<String>,
    #[serde(default, deserialize_with = "lenient::string_map")]
    pub participant_roles: BTreeMap<String, String>,
    /// The robot data documents (`r+<name>`) by name, as XML.
    #[serde(default, deserialize_with = "lenient::string_map")]
    pub data_documents: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub creator: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub root_blip_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub wave_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub wavelet_id: Option<String>,
    /// Absent before protocol 0.22.
    #[serde(default)]
    pub root_thread: Option<BlipThread>,
}

impl Default for WaveletData {
    /// `WaveletData()`.
    fn default() -> Self {
        WaveletData {
            creation_time: -1,
            last_modified_time: -1,
            version: -1,
            participants: Vec::new(),
            participant_roles: BTreeMap::new(),
            data_documents: BTreeMap::new(),
            tags: Vec::new(),
            creator: None,
            root_blip_id: None,
            title: None,
            wave_id: None,
            wavelet_id: None,
            root_thread: None,
        }
    }
}

impl WaveletData {
    /// The wavelet data a client sends to create a wavelet
    /// (`WaveletData(waveId, waveletId, rootBlipId, rootThread)`).
    pub fn new(
        wave_id: &str,
        wavelet_id: &str,
        root_blip_id: &str,
        root_thread: Option<BlipThread>,
    ) -> Self {
        WaveletData {
            wave_id: Some(wave_id.to_owned()),
            wavelet_id: Some(wavelet_id.to_owned()),
            root_blip_id: Some(root_blip_id.to_owned()),
            root_thread,
            ..WaveletData::default()
        }
    }
}

/// How `document.modify` changes what its query matched (`ModifyHow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModifyHow {
    Delete,
    Replace,
    Insert,
    InsertAfter,
    Annotate,
    ClearAnnotation,
    UpdateElement,
}

/// An annotation applied to text inserted by `document.modify`
/// (`BundledAnnotation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundledAnnotation {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub key: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub value: Option<String>,
}

impl BundledAnnotation {
    pub fn new(key: &str, value: &str) -> Self {
        BundledAnnotation {
            key: Some(key.to_owned()),
            value: Some(value.to_owned()),
        }
    }
}

/// What `document.modify` does at each match (`DocumentModifyAction`).
/// Values and elements are used round-robin over the matches.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentModifyAction {
    #[serde(default)]
    pub modify_how: Option<ModifyHow>,
    #[serde(default)]
    pub values: Option<Vec<Option<String>>>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub annotation_key: Option<String>,
    #[serde(default)]
    pub elements: Option<Vec<Option<Element>>>,
    #[serde(default)]
    pub bundled_annotations: Option<Vec<BundledAnnotation>>,
    #[serde(default, deserialize_with = "lenient::boolean")]
    pub use_markup: bool,
}

impl DocumentModifyAction {
    pub fn new(modify_how: ModifyHow) -> Self {
        DocumentModifyAction {
            modify_how: Some(modify_how),
            ..Default::default()
        }
    }

    /// An action with text values.
    pub fn with_values<S: Into<String>>(
        modify_how: ModifyHow,
        values: impl IntoIterator<Item = S>,
    ) -> Self {
        DocumentModifyAction {
            modify_how: Some(modify_how),
            values: Some(values.into_iter().map(|v| Some(v.into())).collect()),
            ..Default::default()
        }
    }

    /// An action with elements.
    pub fn with_elements(
        modify_how: ModifyHow,
        elements: impl IntoIterator<Item = Element>,
    ) -> Self {
        DocumentModifyAction {
            modify_how: Some(modify_how),
            elements: Some(elements.into_iter().map(Some).collect()),
            ..Default::default()
        }
    }

    /// An annotate (or clear-annotation) action.
    pub fn annotation(modify_how: ModifyHow, key: &str, value: Option<&str>) -> Self {
        DocumentModifyAction {
            modify_how: Some(modify_how),
            values: value.map(|v| vec![Some(v.to_owned())]),
            annotation_key: Some(key.to_owned()),
            ..Default::default()
        }
    }

    /// `getValue`.
    pub fn value(&self, index: usize) -> Option<&str> {
        let values = self.values.as_ref().filter(|v| !v.is_empty())?;
        values[index % values.len()].as_deref()
    }

    /// `getElement`.
    pub fn element(&self, index: usize) -> Option<&Element> {
        let elements = self.elements.as_ref().filter(|e| !e.is_empty())?;
        elements[index % elements.len()].as_ref()
    }

    /// `hasTextAt`.
    pub fn has_text_at(&self, index: usize) -> bool {
        self.value(index).is_some()
    }
}

/// Where `document.modify` acts (`DocumentModifyQuery`): at elements of a
/// type with given property values, or at occurrences of a text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentModifyQuery {
    #[serde(default)]
    pub element_match: Option<ElementType>,
    /// The maximum number of matches; negative for no limit.
    #[serde(default = "minus_one_i32", deserialize_with = "lenient::int32")]
    pub max_res: i32,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub text_match: Option<String>,
    #[serde(default)]
    pub restrictions: Option<BTreeMap<String, String>>,
}

impl Default for DocumentModifyQuery {
    fn default() -> Self {
        DocumentModifyQuery {
            element_match: None,
            max_res: -1,
            text_match: None,
            restrictions: None,
        }
    }
}

impl DocumentModifyQuery {
    pub fn element(
        element_match: ElementType,
        restrictions: BTreeMap<String, String>,
        max_res: i32,
    ) -> Self {
        DocumentModifyQuery {
            element_match: Some(element_match),
            max_res,
            text_match: None,
            restrictions: Some(restrictions),
        }
    }

    pub fn text(text_match: &str, max_res: i32) -> Self {
        DocumentModifyQuery {
            element_match: None,
            max_res,
            text_match: Some(text_match.to_owned()),
            restrictions: None,
        }
    }
}

/// One wave in a search result (`SearchResult.Digest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDigest {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub snippet: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub wave_id: Option<String>,
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub last_modified: i64,
    #[serde(default = "minus_one_i64", deserialize_with = "lenient::int64")]
    pub created: i64,
    #[serde(default = "minus_one_i32", deserialize_with = "lenient::int32")]
    pub unread_count: i32,
    #[serde(default = "minus_one_i32", deserialize_with = "lenient::int32")]
    pub blip_count: i32,
    #[serde(default, deserialize_with = "lenient::string_list")]
    pub participants: Vec<String>,
}

/// The result of `robot.search` (`SearchResult`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub query: Option<String>,
    #[serde(default, deserialize_with = "lenient::int32")]
    pub num_results: i32,
    #[serde(default, deserialize_with = "lenient::null_default")]
    pub digests: Vec<SearchDigest>,
}

impl SearchResult {
    pub fn new(query: &str) -> Self {
        SearchResult {
            query: Some(query.to_owned()),
            num_results: 0,
            digests: Vec::new(),
        }
    }

    /// `addDigest`.
    pub fn add_digest(&mut self, digest: SearchDigest) {
        self.num_results += 1;
        self.digests.push(digest);
    }
}

/// A participant's profile (`ParticipantProfile`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantProfile {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub address: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub image_url: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub profile_url: Option<String>,
}

impl ParticipantProfile {
    pub fn new(address: &str, name: &str, image_url: &str, profile_url: &str) -> Self {
        ParticipantProfile {
            address: Some(address.to_owned()),
            name: Some(name.to_owned()),
            image_url: Some(image_url.to_owned()),
            profile_url: Some(profile_url.to_owned()),
        }
    }
}

/// The parameter of `robot.fetchProfiles` (`FetchProfilesRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchProfilesRequest {
    #[serde(default)]
    pub participant_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub language: Option<String>,
}

/// The result of `robot.fetchProfiles` (`FetchProfilesResult`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchProfilesResult {
    #[serde(default)]
    pub profiles: Option<Vec<ParticipantProfile>>,
}

/// An attachment in transit for import and export (`RawAttachmentData`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAttachmentData {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub file_name: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub creator: Option<String>,
    #[serde(default, with = "lenient::base64_bytes")]
    pub data: Vec<u8>,
}

/// A wavelet id as Gson writes the model's `WaveletId`: its two fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiWaveletId {
    pub domain: String,
    pub id: String,
}

fn minus_one_i32() -> i32 {
    -1
}

fn minus_one_i64() -> i64 {
    -1
}

fn newline() -> String {
    "\n".to_owned()
}

/// Blip content; `null` reads as the default.
fn content<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(lenient::opt_string(d)?.unwrap_or_else(newline))
}
