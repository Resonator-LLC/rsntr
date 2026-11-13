//! Tests of the conversation model, ported from `ConversationTestBase`,
//! `WaveletBasedConversationTest`, `ConversationViewTestBase`,
//! `WaveBasedConversationViewTest`, `DocumentBasedManifest*Test`,
//! `BlipIteratorsTest`, `TitleHelperTest` and `TagsDocumentTest`. Tests of
//! listener events are not ported: there are no listeners.

use super::title::{self, TITLE_KEY};
use super::*;
use crate::doc::{
    AnnotationReader, DocError, DocView, MutableDocument, StandaloneDocument, XmlFragment,
};
use crate::docop::BootstrapDocument;
use crate::id::{IdGeneratorImpl, ParticipantId, WaveId, WaveletId};
use crate::wavelet::{WaveView, WaveletError};

type View = WaveBasedConversationView<IdGeneratorImpl>;

fn alice() -> ParticipantId {
    ParticipantId::of("alice@example.com").unwrap()
}

fn bob() -> ParticipantId {
    ParticipantId::of("bob@example.com").unwrap()
}

fn empty_view() -> View {
    let wave = WaveView::new(WaveId::of("example.com", "w+wave").unwrap(), alice(), 42);
    WaveBasedConversationView::new(wave, IdGeneratorImpl::new("example.com", || "s".to_owned()))
}

/// A view with an empty root conversation.
fn view() -> View {
    let mut view = empty_view();
    view.create_root().unwrap();
    view
}

fn root_id() -> WaveletId {
    WaveletId::of("example.com", "conv+root").unwrap()
}

fn root(view: &mut View) -> WaveletBasedConversation<'_> {
    view.root_mut().unwrap()
}

fn doc_xml(view: &View, doc_id: &str) -> String {
    view.wave().root().unwrap().document(doc_id).to_xml_string()
}

fn manifest_xml(view: &View) -> String {
    doc_xml(view, "conversation")
}

fn edit_manifest(
    view: &mut View,
    edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<(), DocError>,
) {
    view.wave_mut()
        .wavelet_mut(&root_id())
        .unwrap()
        .edit_document("conversation", edit)
        .unwrap();
}

fn blip_ids(thread: &ConversationThread) -> Vec<&str> {
    thread.blips().iter().map(|blip| blip.id()).collect()
}

fn thread_ids(blip: &ConversationBlip) -> Vec<&str> {
    blip.reply_threads().iter().map(|t| t.id()).collect()
}

fn located(view: &View, blip_id: &str) -> Vec<(String, Option<usize>)> {
    let conversation = view.root().unwrap();
    let blip = conversation.blip(blip_id).unwrap();
    conversation
        .locate_reply_threads(blip)
        .iter()
        .map(|reply| (reply.thread.id().to_owned(), reply.location))
        .collect()
}

// ---- Conversation view ----

#[test]
fn empty_view_has_no_conversations() {
    let view = empty_view();
    assert!(view.conversations().is_empty());
    assert!(view.root().is_none());
    assert_eq!("example.com/w+wave", view.id());
}

#[test]
fn created_root_accessible_by_get_root() {
    let view = view();
    let root = view.root().unwrap();
    assert_eq!("example.com/conv+root", root.id());
    assert_eq!(&root_id(), root.wavelet_id());
    assert_eq!(1, view.conversations().len());
    assert_eq!(&[alice()], root.participants());
}

#[test]
fn duplicate_create_root_fails() {
    let mut view = view();
    let before = manifest_xml(&view);
    assert!(matches!(
        view.create_root().map(drop),
        Err(ConversationError::Wavelet(WaveletError::DuplicateWavelet(
            _
        )))
    ));
    assert_eq!(before, manifest_xml(&view));
}

#[test]
fn create_conversations() {
    let mut view = view();
    let first = view
        .create_conversation()
        .unwrap()
        .wavelet()
        .data()
        .wavelet_id()
        .clone();
    let second = view
        .create_conversation()
        .unwrap()
        .wavelet()
        .data()
        .wavelet_id()
        .clone();
    assert_ne!(first, second);
    assert!(first.id().starts_with("conv+"));
    assert_ne!(root_id(), first);
    assert_eq!(3, view.conversations().len());
    assert!(view.conversation(&first).unwrap().is_some());
    assert_eq!(
        first,
        *view
            .conversation_by_id(&conversation_id_for(&first))
            .unwrap()
            .unwrap()
            .wavelet_id()
    );
}

#[test]
fn non_existent_conversation_is_none() {
    let view = view();
    let missing = WaveletId::of("example.com", "conv+missing").unwrap();
    assert!(view.conversation(&missing).unwrap().is_none());
    let not_conversational = WaveletId::of("example.com", "user+alice@example.com").unwrap();
    assert!(view.conversation(&not_conversational).is_err());
}

#[test]
fn delete_conversation_from_view() {
    let mut view = view();
    let blip = root(&mut view).append_blip("").unwrap();
    root(&mut view).delete().unwrap();
    assert!(view.root().is_none());
    assert!(view.conversations().is_empty());
    assert!(view.root_mut().is_err());
    assert_eq!("", manifest_xml(&view));
    assert_eq!("", doc_xml(&view, &blip));
    // The wavelet remains, and can be made conversational again.
    let wavelet = view.wave_mut().wavelet_mut(&root_id()).unwrap();
    make_wavelet_conversational(wavelet).unwrap();
    assert!(view.root().is_some());
}

#[test]
fn manifest_erasure_removes_conversation() {
    let mut view = view();
    edit_manifest(&mut view, manifest::delete_manifest);
    assert!(view.root().is_none());
}

// ---- Wavelets and manifests ----

#[test]
fn new_wavelet_has_no_conversation() {
    let mut view = empty_view();
    let wavelet = view.wave_mut().create_root().unwrap();
    assert!(!wavelet_has_conversation(wavelet.data()));
    assert!(Conversation::read(wavelet.data()).is_none());
    make_wavelet_conversational(wavelet).unwrap();
    assert!(wavelet_has_conversation(wavelet.data()));
    assert_eq!(
        "<conversation/>",
        wavelet.document("conversation").to_xml_string()
    );
    // Not twice.
    assert!(make_wavelet_conversational(wavelet).is_err());
}

#[test]
fn extra_conversation_element_does_not_fail() {
    let mut view = view();
    let blip = root(&mut view).append_blip("").unwrap();
    edit_manifest(&mut view, |doc| {
        doc.create_child_element(doc.view().root(), "conversation", &[])
            .map(drop)
    });
    // The first element stays the manifest.
    assert_eq!(
        vec![blip.as_str()],
        blip_ids(view.root().unwrap().root_thread())
    );
    let second = root(&mut view).append_blip("").unwrap();
    assert_eq!(
        format!(
            "<conversation><blip id=\"{blip}\"/><blip id=\"{second}\"/></conversation><conversation/>"
        ),
        manifest_xml(&view)
    );
}

#[test]
fn missing_anchor_attributes_means_no_anchor() {
    let mut view = view();
    let blip = root(&mut view).append_blip("").unwrap();
    let other = view
        .create_conversation()
        .unwrap()
        .wavelet()
        .data()
        .wavelet_id()
        .clone();
    assert_eq!(None, view.anchor_of(&other));
    // Only one of the two attributes.
    let other_wavelet = view.wave_mut().wavelet_mut(&other).unwrap();
    other_wavelet
        .edit_document("conversation", |doc| {
            let top = doc.view().first_child_element(doc.view().root()).unwrap();
            doc.set_element_attribute(top, "anchorBlip", Some(&blip))
        })
        .unwrap();
    assert_eq!(None, view.anchor_of(&other));
    let anchor = view.conversation(&other).unwrap().unwrap().anchor().clone();
    assert_eq!(None, anchor.conversation_id);
    assert_eq!(Some(blip), anchor.blip_id);
}

#[test]
fn set_and_clear_anchor_updates_manifest() {
    let mut view = view();
    let blip = root(&mut view).append_blip("").unwrap();
    let other = view
        .create_conversation()
        .unwrap()
        .wavelet()
        .data()
        .wavelet_id()
        .clone();
    view.set_anchor(&other, Some((&root_id(), &blip))).unwrap();
    assert_eq!(Some((root_id(), blip.clone())), view.anchor_of(&other));
    let xml = view
        .wave()
        .wavelet(&other)
        .unwrap()
        .document("conversation")
        .to_xml_string();
    assert_eq!(
        format!("<conversation anchorBlip=\"{blip}\" anchorWavelet=\"example.com/conv+root\"/>"),
        xml
    );
    // The anchor survives more blips in the anchor conversation.
    root(&mut view).append_blip("").unwrap();
    assert_eq!(Some((root_id(), blip.clone())), view.anchor_of(&other));
    // An anchor at a blip that does not exist is no anchor.
    view.set_anchor(&other, Some((&root_id(), "b+nope")))
        .unwrap();
    assert_eq!(None, view.anchor_of(&other));
    view.set_anchor(&other, None).unwrap();
    assert_eq!(None, view.anchor_of(&other));
    let xml = view
        .wave()
        .wavelet(&other)
        .unwrap()
        .document("conversation")
        .to_xml_string();
    assert_eq!("<conversation/>", xml);
}

#[test]
fn anchor_to_self_fails() {
    let mut view = view();
    let blip = root(&mut view).append_blip("").unwrap();
    assert!(
        view.set_anchor(&root_id(), Some((&root_id(), &blip)))
            .is_err()
    );
}

#[test]
fn participants_are_a_set() {
    let mut view = view();
    root(&mut view).add_participant(&bob()).unwrap();
    root(&mut view).add_participant(&bob()).unwrap();
    assert_eq!(&[alice(), bob()], view.root().unwrap().participants());
    root(&mut view).remove_participant(&bob()).unwrap();
    root(&mut view).remove_participant(&bob()).unwrap();
    assert_eq!(&[alice()], view.root().unwrap().participants());
    // Create, add, add (no-op), remove, remove (no-op): add alice, manifest,
    // add bob, remove bob.
    assert_eq!(4, view.wave().root().unwrap().ops().len());
}

#[test]
fn blip_metadata_matches_wavelet() {
    let mut view = view();
    let blip = root(&mut view).append_blip("").unwrap();
    let conversation = view.root().unwrap();
    let data = conversation.blip_data(&blip).unwrap();
    assert_eq!(&alice(), data.author());
    assert_eq!(&[alice()], data.contributors());
    assert_eq!(42, data.last_modified_time());
    assert_eq!("<body><line/></body>", data.content().to_xml_string());
    assert_eq!(blip, data.id());
}

#[test]
fn append_blips_updates_manifest() {
    let mut view = view();
    assert!(view.root().unwrap().root_thread().blips().is_empty());
    assert!(view.root().unwrap().root_thread().first_blip().is_none());
    let b1 = root(&mut view).append_blip("").unwrap();
    let b2 = root(&mut view).append_blip("").unwrap();
    assert_eq!(
        format!("<conversation><blip id=\"{b1}\"/><blip id=\"{b2}\"/></conversation>"),
        manifest_xml(&view)
    );
    let conversation = view.root().unwrap();
    assert_eq!(
        vec![b1.as_str(), b2.as_str()],
        blip_ids(conversation.root_thread())
    );
    assert!(conversation.is_root_blip(&b1));
    assert!(!conversation.is_root_blip(&b2));
    assert_eq!("", conversation.thread_of(&b1).unwrap().id());
    assert!(conversation.blip("foobar").is_none());
}

#[test]
fn insert_blip_before_first_and_between() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let b0 = root(&mut view).insert_blip("", &b1).unwrap();
    let b2 = root(&mut view).append_blip("").unwrap();
    let b15 = root(&mut view).insert_blip("", &b2).unwrap();
    let conversation = view.root().unwrap();
    assert_eq!(
        vec![b0.as_str(), b1.as_str(), b15.as_str(), b2.as_str()],
        blip_ids(conversation.root_thread())
    );
    assert!(conversation.is_root_blip(&b0));
    // The successor must be in the thread.
    let thread = root(&mut view).add_reply_thread(&b1).unwrap();
    assert!(root(&mut view).insert_blip(&thread, &b2).is_err());
    assert!(root(&mut view).insert_blip("", "b+nope").is_err());
}

#[test]
fn append_replies_updates_manifest() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&b1).unwrap();
    // An empty thread is a thread.
    assert_eq!(
        vec![t1.as_str()],
        thread_ids(view.root().unwrap().blip(&b1).unwrap())
    );
    let r1 = root(&mut view).append_blip(&t1).unwrap();
    // The thread took the id the next blip then got.
    assert_eq!(t1, r1);
    let r2 = root(&mut view).append_blip(&t1).unwrap();
    let t2 = root(&mut view).add_reply_thread(&b1).unwrap();
    assert_eq!(
        format!(
            "<conversation><blip id=\"{b1}\"><thread id=\"{t1}\"><blip id=\"{r1}\"/>\
             <blip id=\"{r2}\"/></thread><thread id=\"{t2}\"/></blip></conversation>"
        ),
        manifest_xml(&view)
    );
    let conversation = view.root().unwrap();
    assert_eq!(
        vec![r1.as_str(), r2.as_str()],
        blip_ids(conversation.thread(&t1).unwrap())
    );
    assert_eq!(b1, conversation.parent_blip_of(&t1).unwrap().id());
    assert!(conversation.parent_blip_of("").is_none());
    assert!(!conversation.thread(&t1).unwrap().is_inline());
    assert_eq!(t1, conversation.thread_of(&r2).unwrap().id());
    assert_eq!(vec![(t1, None), (t2, None)], located(&view, &b1));
}

#[test]
fn append_inline_reply_creates_anchor() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| doc.insert_text(3, "0123456789"))
        .unwrap();
    let t1 = root(&mut view).add_inline_reply_thread(&b1, 7).unwrap();
    assert_eq!(
        format!("<body><line/>0123<reply id=\"{t1}\"/>456789</body>"),
        doc_xml(&view, &b1)
    );
    assert_eq!(
        format!(
            "<conversation><blip id=\"{b1}\"><thread id=\"{t1}\" inline=\"true\"/></blip>\
             </conversation>"
        ),
        manifest_xml(&view)
    );
    assert!(view.root().unwrap().thread(&t1).unwrap().is_inline());
    assert_eq!(vec![(t1.clone(), Some(7))], located(&view, &b1));
    // The location follows the content.
    root(&mut view)
        .edit_blip(&b1, |doc| doc.insert_text(3, "abc"))
        .unwrap();
    assert_eq!(vec![(t1.clone(), Some(10))], located(&view, &b1));
    // An invalid location fails, leaving nothing behind.
    assert!(root(&mut view).add_inline_reply_thread(&b1, 1000).is_err());
    assert_eq!(
        1,
        view.root()
            .unwrap()
            .blip(&b1)
            .unwrap()
            .reply_threads()
            .len()
    );
}

#[test]
fn inline_reply_with_multiple_anchors_uses_first() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| doc.insert_text(3, "0123456789"))
        .unwrap();
    let t1 = root(&mut view).add_inline_reply_thread(&b1, 9).unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| {
            doc.create_element(5, "reply", &[("id", &t1)]).map(drop)
        })
        .unwrap();
    assert_eq!(vec![(t1, Some(5))], located(&view, &b1));
}

#[test]
fn inline_reply_with_deleted_anchor_has_invalid_location() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_inline_reply_thread(&b1, 3).unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| {
            let anchor = doc.view().element_starting_at(3).unwrap();
            doc.delete_node(anchor)
        })
        .unwrap();
    assert_eq!(vec![(t1, None)], located(&view, &b1));
}

#[test]
fn inline_replies_in_location_order() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| doc.insert_text(3, "0123456789"))
        .unwrap();
    let late = root(&mut view).add_inline_reply_thread(&b1, 9).unwrap();
    root(&mut view).append_blip(&late).unwrap();
    let none = root(&mut view).add_reply_thread(&b1).unwrap();
    root(&mut view).append_blip(&none).unwrap();
    let early = root(&mut view).add_inline_reply_thread(&b1, 4).unwrap();
    let conversation = view.root().unwrap();
    // Manifest order is creation order.
    assert_eq!(
        vec![late.as_str(), none.as_str(), early.as_str()],
        thread_ids(conversation.blip(&b1).unwrap())
    );
    assert_eq!(
        vec![(early, Some(4)), (late, Some(11)), (none, None)],
        located(&view, &b1)
    );
}

#[test]
fn delete_single_root_thread_blip_removes_it() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    root(&mut view).delete_blip(&b1).unwrap();
    assert!(view.root().unwrap().root_thread().blips().is_empty());
    assert!(view.root().unwrap().blip(&b1).is_none());
    assert_eq!("<conversation/>", manifest_xml(&view));
    // The document remains, empty.
    assert!(view.wave().root().unwrap().has_document(&b1));
    assert_eq!("", doc_xml(&view, &b1));
    // Deleting again: not found.
    assert_eq!(
        Err(ConversationError::NoSuchBlip(b1.clone())),
        root(&mut view).delete_blip(&b1)
    );
    assert!(root(&mut view).edit_blip(&b1, |_| Ok(())).is_err());
    // Can append after deleting.
    let b2 = root(&mut view).append_blip("").unwrap();
    assert_eq!(
        vec![b2.as_str()],
        blip_ids(view.root().unwrap().root_thread())
    );
}

#[test]
fn delete_blip_in_thread_leaves_siblings() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let b2 = root(&mut view).append_blip("").unwrap();
    let b3 = root(&mut view).append_blip("").unwrap();
    root(&mut view).delete_blip(&b2).unwrap();
    assert_eq!(
        vec![b1.as_str(), b3.as_str()],
        blip_ids(view.root().unwrap().root_thread())
    );
}

#[test]
fn delete_last_blip_in_non_root_thread_deletes_thread() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&b1).unwrap();
    let r1 = root(&mut view).append_blip(&t1).unwrap();
    let r2 = root(&mut view).append_blip(&t1).unwrap();
    root(&mut view).delete_blip(&r1).unwrap();
    assert_eq!(
        vec![r2.as_str()],
        blip_ids(view.root().unwrap().thread(&t1).unwrap())
    );
    root(&mut view).delete_blip(&r2).unwrap();
    assert!(view.root().unwrap().thread(&t1).is_none());
    assert_eq!(
        format!("<conversation><blip id=\"{b1}\"/></conversation>"),
        manifest_xml(&view)
    );
    assert_eq!(
        Err(ConversationError::NoSuchThread(t1.clone())),
        root(&mut view).append_blip(&t1)
    );
    // Can reply again.
    let t2 = root(&mut view).add_reply_thread(&b1).unwrap();
    root(&mut view).append_blip(&t2).unwrap();
    assert_eq!(
        vec![t2.as_str()],
        thread_ids(view.root().unwrap().blip(&b1).unwrap())
    );
}

#[test]
fn delete_blip_with_replies_deletes_replies() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let b2 = root(&mut view).append_blip("").unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| doc.insert_text(3, "0123456789"))
        .unwrap();
    let t1 = root(&mut view).add_inline_reply_thread(&b1, 5).unwrap();
    let r1 = root(&mut view).append_blip(&t1).unwrap();
    let t2 = root(&mut view).add_reply_thread(&b1).unwrap();
    let r2 = root(&mut view).append_blip(&t2).unwrap();
    let r3 = root(&mut view).append_blip(&t2).unwrap();
    // A reply to the inline reply.
    let t3 = root(&mut view).add_reply_thread(&r1).unwrap();
    let r4 = root(&mut view).append_blip(&t3).unwrap();
    root(&mut view).delete_blip(&b1).unwrap();
    assert_eq!(
        format!("<conversation><blip id=\"{b2}\"/></conversation>"),
        manifest_xml(&view)
    );
    for blip in [&b1, &r1, &r2, &r3, &r4] {
        assert_eq!("", doc_xml(&view, blip), "{blip}");
        assert!(view.root().unwrap().blip(blip).is_none());
    }
    for thread in [&t1, &t2, &t3] {
        assert!(view.root().unwrap().thread(thread).is_none());
    }
    assert!(view.root().unwrap().is_root_blip(&b2));
}

#[test]
fn delete_inline_reply_deletes_anchor() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_inline_reply_thread(&b1, 3).unwrap();
    let r1 = root(&mut view).append_blip(&t1).unwrap();
    let t2 = root(&mut view).add_inline_reply_thread(&b1, 3).unwrap();
    root(&mut view).append_blip(&t2).unwrap();
    root(&mut view).delete_blip(&r1).unwrap();
    assert_eq!(
        format!("<body><line/><reply id=\"{t2}\"/></body>"),
        doc_xml(&view, &b1)
    );
    root(&mut view).delete_thread(&t2).unwrap();
    assert_eq!("<body><line/></body>", doc_xml(&view, &b1));
    assert_eq!(
        format!("<conversation><blip id=\"{b1}\"/></conversation>"),
        manifest_xml(&view)
    );
}

#[test]
fn delete_root_thread_removes_all_blips() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&b1).unwrap();
    root(&mut view).append_blip(&t1).unwrap();
    root(&mut view).append_blip("").unwrap();
    root(&mut view).delete_thread("").unwrap();
    assert_eq!("<conversation/>", manifest_xml(&view));
    // The root thread remains usable.
    assert!(view.root().unwrap().root_thread().blips().is_empty());
    root(&mut view).append_blip("").unwrap();
}

#[test]
fn delete_non_root_and_empty_threads() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&b1).unwrap();
    let r1 = root(&mut view).append_blip(&t1).unwrap();
    root(&mut view).append_blip(&t1).unwrap();
    let empty = root(&mut view).add_reply_thread(&b1).unwrap();
    root(&mut view).delete_thread(&t1).unwrap();
    assert!(view.root().unwrap().blip(&r1).is_none());
    assert_eq!(
        vec![empty.as_str()],
        thread_ids(view.root().unwrap().blip(&b1).unwrap())
    );
    root(&mut view).delete_thread(&empty).unwrap();
    assert_eq!(
        format!("<conversation><blip id=\"{b1}\"/></conversation>"),
        manifest_xml(&view)
    );
    assert_eq!(
        Err(ConversationError::NoSuchThread(empty.clone())),
        root(&mut view).delete_thread(&empty)
    );
}

#[test]
fn data_documents() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    root(&mut view)
        .edit_data_document("some-doc", |doc| doc.insert_text(0, "x"))
        .unwrap();
    assert_eq!("x", doc_xml(&view, "some-doc"));
    assert!(root(&mut view).edit_data_document(&b1, |_| Ok(())).is_err());
    assert!(
        root(&mut view)
            .edit_data_document("b+other", |_| Ok(()))
            .is_err()
    );
    assert!(
        root(&mut view)
            .edit_data_document("conversation", |_| Ok(()))
            .is_err()
    );
}

#[test]
fn empty_manifest_thread_not_ignored() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&b1).unwrap();
    let conversation = view.root().unwrap();
    assert_eq!(
        t1,
        conversation
            .root_thread()
            .first_blip()
            .unwrap()
            .reply_threads()[0]
            .id()
    );
    assert!(conversation.thread(&t1).unwrap().blips().is_empty());
}

#[test]
fn blip_missing_from_wavelet_is_skipped() {
    let mut view = view();
    // Only in the manifest.
    edit_manifest(&mut view, |doc| {
        let top = doc.view().first_child_element(doc.view().root()).unwrap();
        doc.create_child_element(top, "blip", &[("id", "b+ghost")])
            .map(drop)
    });
    assert!(view.root().unwrap().root_thread().first_blip().is_none());
    assert!(view.root().unwrap().blip("b+ghost").is_none());
    let first = root(&mut view).append_blip("").unwrap();
    edit_manifest(&mut view, |doc| {
        let top = doc.view().first_child_element(doc.view().root()).unwrap();
        doc.create_child_element(top, "blip", &[("id", "b+ghost2")])
            .map(drop)
    });
    let third = root(&mut view).append_blip("").unwrap();
    assert_eq!(
        4,
        read_manifest(view.wave().root().unwrap().data())
            .unwrap()
            .root_thread
            .blips
            .len()
    );
    let conversation = view.root().unwrap();
    assert_eq!(
        vec![first.as_str(), third.as_str()],
        blip_ids(conversation.root_thread())
    );
    // The first blip with a document is the root blip.
    assert!(conversation.is_root_blip(&first));
    // Deleting the root thread deletes only the blips with documents.
    root(&mut view).delete_thread("").unwrap();
    assert_eq!(
        "<conversation><blip id=\"b+ghost\"/><blip id=\"b+ghost2\"/></conversation>",
        manifest_xml(&view)
    );
}

#[test]
fn concurrent_deletion_of_final_blips_leaves_empty_thread() {
    let mut view = view();
    let first = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&first).unwrap();
    let b1 = root(&mut view).append_blip(&t1).unwrap();
    let b2 = root(&mut view).append_blip(&t1).unwrap();
    root(&mut view).delete_blip(&b1).unwrap();
    // Someone else removes the last blip's manifest entry.
    let location = view.root().unwrap().blip(&b2).unwrap().manifest_location;
    edit_manifest(&mut view, |doc| {
        let element = doc.view().element_starting_at(location).unwrap();
        doc.delete_node(element)
    });
    let conversation = view.root().unwrap();
    assert!(conversation.blip(&b2).is_none());
    assert!(conversation.thread(&t1).unwrap().blips().is_empty());
    assert_eq!(
        vec![t1.as_str()],
        thread_ids(conversation.blip(&first).unwrap())
    );
    root(&mut view).append_blip("").unwrap();
    assert!(view.root().unwrap().thread(&t1).is_some());
}

#[test]
fn malformed_manifest_gives_degraded_view() {
    let mut view = view();
    let real = root(&mut view).append_blip("").unwrap();
    edit_manifest(&mut view, |doc| {
        // Junk around and inside the manifest, a blip and a thread without
        // id, a blip under a blip, a thread under a thread.
        let xml = XmlFragment::parse(
            "<junk/><blip/><blip id=\"b+ghost\"><thread id=\"t\"><blip id=\"b+x\"/></thread></blip>",
        )?;
        let end = doc.view().size() - 1;
        doc.insert_xml(end, &xml)?;
        doc.insert_text(1, "text")?;
        let blip = doc
            .view()
            .element_with_tag_name("blip", doc.view().root())
            .ok_or(DocError::InvalidNode)?;
        let inner = XmlFragment::parse(
            "<blip id=\"b+nested\"/><thread><blip id=\"b+y\"/></thread>\
             <thread id=\"ok\" inline=\"TRUE\"><thread id=\"deep\"/></thread>",
        )?;
        doc.insert_xml(doc.view().content_end(blip), &inner)
            .map(drop)
    });
    let conversation = view.root().unwrap();
    assert_eq!(vec![real.as_str()], blip_ids(conversation.root_thread()));
    let blip = conversation.blip(&real).unwrap();
    // The thread without id is left out; "inline" is parsed leniently.
    assert_eq!(vec!["ok"], thread_ids(blip));
    assert!(blip.reply_threads()[0].is_inline());
    assert!(blip.reply_threads()[0].blips().is_empty());
    assert!(conversation.thread("deep").is_none());
    assert_eq!(
        vec![&real],
        conversation
            .blips_breadth_first()
            .iter()
            .map(|b| b.id())
            .collect::<Vec<_>>()
    );
    // Mutations still work on what is there.
    root(&mut view).append_blip("ok").unwrap();
    root(&mut view).delete_blip(&real).unwrap();
    assert!(view.root().unwrap().root_thread().blips().is_empty());
}

#[test]
fn duplicate_manifest_entries() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    edit_manifest(&mut view, |doc| {
        let top = doc.view().first_child_element(doc.view().root()).unwrap();
        doc.create_child_element(top, "blip", &[("id", &b1)])
            .map(drop)
    });
    let conversation = view.root().unwrap();
    assert_eq!(
        vec![b1.as_str(), b1.as_str()],
        blip_ids(conversation.root_thread())
    );
    // The last entry wins.
    let location = conversation.blip(&b1).unwrap().manifest_location;
    assert_eq!(
        conversation.root_thread().blips()[1].manifest_location,
        location
    );
    root(&mut view).delete_blip(&b1).unwrap();
    // The last entry is gone. The document still exists, though empty, so
    // the other entry still is a blip, as it would be in the original.
    assert_eq!(
        format!("<conversation><blip id=\"{b1}\"/></conversation>"),
        manifest_xml(&view)
    );
    assert_eq!(
        vec![b1.as_str()],
        blip_ids(view.root().unwrap().root_thread())
    );
    assert_eq!("", doc_xml(&view, &b1));
}

#[test]
fn manifest_read() {
    let mut view = view();
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_inline_reply_thread(&b1, 3).unwrap();
    let r1 = root(&mut view).append_blip(&t1).unwrap();
    let manifest = read_manifest(view.wave().root().unwrap().data()).unwrap();
    assert_eq!(AnchorData::default(), manifest.anchor);
    assert_eq!(None, manifest.root_thread.id);
    assert!(!manifest.root_thread.inline);
    let blip = &manifest.root_thread.blips[0];
    assert_eq!(Some(b1), blip.id);
    assert_eq!(1, blip.location);
    let thread = &blip.threads[0];
    assert_eq!(Some(t1), thread.id);
    assert!(thread.inline);
    assert_eq!(Some(r1), thread.blips[0].id);
    assert!(thread.blips[0].threads.is_empty());
    assert!(Manifest::read(&BootstrapDocument::new()).is_none());
}

// ---- BlipIterators ----

#[test]
fn blips_breadth_first() {
    let mut view = view();
    let ids = |view: &View| -> Vec<String> {
        view.root()
            .unwrap()
            .blips_breadth_first()
            .iter()
            .map(|blip| blip.id().to_owned())
            .collect()
    };
    assert!(ids(&view).is_empty());
    let b1 = root(&mut view).append_blip("").unwrap();
    let t1 = root(&mut view).add_reply_thread(&b1).unwrap();
    let b1t1b1 = root(&mut view).append_blip(&t1).unwrap();
    let t11 = root(&mut view).add_reply_thread(&b1t1b1).unwrap();
    let b1t1b1t1b1 = root(&mut view).append_blip(&t11).unwrap();
    let b1t1b2 = root(&mut view).append_blip(&t1).unwrap();
    let t2 = root(&mut view).add_reply_thread(&b1).unwrap();
    let b1t2b1 = root(&mut view).append_blip(&t2).unwrap();
    let b2 = root(&mut view).append_blip("").unwrap();
    let t3 = root(&mut view).add_reply_thread(&b2).unwrap();
    let b2t1b1 = root(&mut view).append_blip(&t3).unwrap();
    assert_eq!(
        vec![b1, b2, b1t1b1, b1t1b2, b1t2b1, b2t1b1, b1t1b1t1b1],
        ids(&view)
    );
}

// ---- TitleHelper ----

fn doc(xml: &str) -> StandaloneDocument {
    let mut doc = StandaloneDocument::default();
    doc.mutable()
        .insert_xml(0, &XmlFragment::parse(xml).unwrap())
        .unwrap();
    doc
}

fn title_of(doc: &StandaloneDocument) -> String {
    use crate::doc::DocumentSink;
    title::extract_title(doc.document())
}

fn has_explicit(doc: &StandaloneDocument) -> bool {
    use crate::doc::DocumentSink;
    title::has_explicit_title(doc.document())
}

fn first_title_change(doc: &StandaloneDocument) -> Option<usize> {
    use crate::doc::DocumentSink;
    let size = DocView::new(doc.document()).size();
    doc.document()
        .find_annotation_change(0, size, TITLE_KEY, None)
}

#[test]
fn empty_title_when_no_annotation() {
    let d = doc("<body><line/>Some text<line/>Some more text</body>");
    assert_eq!("", title_of(&d));
    assert!(!has_explicit(&d));
    assert_eq!("", title_of(&doc("Just text")));
}

#[test]
fn explicit_value_overrides() {
    let mut d = doc("<body><line/>Some text<line/>Some more text</body>");
    d.mutable()
        .set_annotation(2, 7, TITLE_KEY, Some("Blah"))
        .unwrap();
    assert_eq!("Blah", title_of(&d));
    assert!(has_explicit(&d));
}

#[test]
fn empty_value_uses_encompassed_text() {
    let mut d = doc("<body><line id=\"1\"/>Some text<line/>Some more text</body>");
    let start_text = 3;
    for i in 0..=start_text {
        d.mutable()
            .reset_annotation(i, start_text + 7, TITLE_KEY, Some(""))
            .unwrap();
        assert_eq!("Some te", title_of(&d));
    }
    d.mutable()
        .reset_annotation(start_text + 1, start_text + 7, TITLE_KEY, Some(""))
        .unwrap();
    assert_eq!("ome te", title_of(&d));
    d.mutable()
        .reset_annotation(start_text + 1, start_text + 13, TITLE_KEY, Some(""))
        .unwrap();
    assert_eq!("ome textSo", title_of(&d));
    assert!(!has_explicit(&d));
}

#[test]
fn setting_title_changes_expliciticity() {
    let mut d = doc("<body><line id=\"1\"/>Some text<line/>Some mor</body>");
    let start_text = 3;
    title::set_explicit_title(&mut d.mutable(), "Blah").unwrap();
    assert_eq!("Blah", title_of(&d));
    assert!(has_explicit(&d));
    title::set_implicit_title(&mut d.mutable(), start_text + 1, start_text + 7).unwrap();
    assert_eq!("ome te", title_of(&d));
    assert!(!has_explicit(&d));
    title::set_explicit_title(&mut d.mutable(), "Blah2").unwrap();
    assert_eq!("Blah2", title_of(&d));
    assert!(has_explicit(&d));
    assert!(title::set_implicit_title(&mut d.mutable(), 4, 4).is_err());
    assert!(title::set_explicit_title(&mut StandaloneDocument::default().mutable(), "x").is_err());
}

#[test]
fn finding_implicit_title_doesnt_override_explicit() {
    let mut d = doc("<body><line/>Some text<line/>Some more text</body>");
    title::set_explicit_title(&mut d.mutable(), "Blah").unwrap();
    let ops = d.ops().len();
    title::maybe_find_and_set_implicit_title(&mut d.mutable()).unwrap();
    assert_eq!(ops, d.ops().len());
    assert_eq!("Blah", title_of(&d));
    assert!(has_explicit(&d));
}

fn implicit_title(xml: &str) -> (String, StandaloneDocument) {
    let mut d = doc(xml);
    title::maybe_find_and_set_implicit_title(&mut d.mutable()).unwrap();
    (title_of(&d), d)
}

#[test]
fn finding_implicit_title_obeys_sentence_and_first_line_boundaries() {
    let (title, d) =
        implicit_title("<body><line id=\"1\"/>Some text<line/>Some more. text?</body>");
    assert_eq!("Some text", title);
    assert!(!has_explicit(&d));
    assert_eq!(Some(1), first_title_change(&d));
    for (expected, first_line) in [
        ("Some text.", "Some text."),
        ("Some text!", "Some text!"),
        ("Some text!!", "Some text!!"),
        ("Some text!?.!", "Some text!?.!"),
        ("The number 5.5 is good", "The number 5.5 is good"),
        ("Some text.", "Some text. and more"),
        ("Some text!?.!", "Some text!?.! and more!"),
        ("Some text!?.!", "Some t<x/>ext!?.! and more!"),
        ("Some text!?.!", "Some text<x/>!?.! and more!"),
        ("Some text!?.!", "Some text!?.! a<x/>nd more!"),
        ("Some text!?.!", "Some te<x>xt!?.! a</x>nd more!"),
        ("Some 5.5 text!?.!", "Some 5.5 text!?.! and more!"),
    ] {
        let xml = format!("<body><line/>{first_line}<line/>Some more text</body>");
        let (title, d) = implicit_title(&xml);
        assert_eq!(expected, title, "{first_line}");
        assert_eq!(Some(1), first_title_change(&d));
        assert!(!has_explicit(&d));
    }
    // An empty first line: the title is empty but marked.
    let (title, d) =
        implicit_title("<body><line/><line/>Some text.and more<line/>Some more text</body>");
    assert_eq!("", title);
    assert_eq!(Some(1), first_title_change(&d));
    use crate::doc::DocumentSink;
    assert_eq!(
        Some(3),
        d.document()
            .find_annotation_change(1, 100, TITLE_KEY, Some(""))
    );
}

#[test]
fn find_implicit_title() {
    use crate::doc::DocumentSink;
    let find = |xml: &str| title::find_implicit_title(&DocView::new(doc(xml).document()));
    assert_eq!(
        Some((1, 13)),
        find("<body><line/>Some text.<line/>Some more text</body>")
    );
    assert_eq!(None, find("<body></body>"));
    assert_eq!(
        None,
        title::find_implicit_title(&DocView::new(&BootstrapDocument::new()))
    );
    // Without a second line or sentence end: up to the last item.
    assert_eq!(Some((1, 7)), find("<body><line/>Some</body>"));
    let (start, end) = find(
        "<body><line/>a!xa!!!!!!!!!!!!!!!!!!!!!!!!!!!!xxxxxxxxxxxxxxxxxxxxxxx<line/>foo</body>",
    )
    .unwrap();
    assert!(end > start);
}

#[test]
fn finding_implicit_title_clears_from_invalid_documents() {
    let (title, mut d) = implicit_title("<body><line/>Some text.<line/>Some more text</body>");
    assert_eq!("Some text.", title);
    let mut m = d.mutable();
    m.empty_element(m.view().root()).unwrap();
    m.append_xml(&XmlFragment::text("Blah. Blah").wrap("x"))
        .unwrap();
    title::maybe_find_and_set_implicit_title(&mut m).unwrap();
    assert_eq!("", title_of(&d));
    assert!(!has_explicit(&d));
    assert_eq!(None, first_title_change(&d));
}

#[test]
fn empty_document_with_title() {
    let mut d = BootstrapDocument::new();
    d.consume(title::empty_document_with_title().as_op())
        .unwrap();
    assert_eq!(
        "<body><?a \"conv/title\"=\"\"?><line/></body><?a \"conv/title\"?>",
        d.to_xml_string()
    );
    let mut plain = StandaloneDocument::default();
    plain
        .mutable()
        .append_xml(&blips::initial_content())
        .unwrap();
    use crate::doc::DocumentSink;
    assert_eq!(DocView::new(plain.document()), DocView::new(&d));
}

#[test]
fn conversation_title() {
    let mut view = view();
    assert_eq!("", view.root().unwrap().title());
    assert!(root(&mut view).set_explicit_title("x").is_err());
    let b1 = root(&mut view).append_blip("").unwrap();
    root(&mut view)
        .edit_blip(&b1, |doc| doc.insert_text(3, "First line. Rest"))
        .unwrap();
    assert_eq!("", view.root().unwrap().title());
    root(&mut view).update_implicit_title().unwrap();
    assert_eq!("First line.", view.root().unwrap().title());
    root(&mut view).set_explicit_title("Named").unwrap();
    assert_eq!("Named", view.root().unwrap().title());
}

// ---- TagsDocument ----

#[test]
fn tags_document() {
    let mut d = StandaloneDocument::default();
    use crate::doc::DocumentSink;
    tags::add_tag(&mut d.mutable(), "tag1").unwrap();
    assert_eq!("<tag>tag1</tag>", d.document().to_xml_string());
    tags::add_tag(&mut d.mutable(), "tag2").unwrap();
    tags::insert_tag(&mut d.mutable(), "tag0", 0).unwrap();
    tags::insert_tag(&mut d.mutable(), "tag15", 2).unwrap();
    tags::insert_tag(&mut d.mutable(), "tag3", 10).unwrap();
    assert_eq!(
        vec!["tag0", "tag1", "tag15", "tag2", "tag3"],
        tags::tags(d.document())
    );
    assert!(tags::delete_tag(&mut d.mutable(), "tag15").unwrap());
    assert!(!tags::delete_tag(&mut d.mutable(), "tag15").unwrap());
    assert_eq!(
        vec!["tag0", "tag1", "tag2", "tag3"],
        tags::tags(d.document())
    );
    // Empty tag elements are not tags, and can be cleaned up.
    let mut m = d.mutable();
    m.create_element(0, "tag", &[]).unwrap();
    m.create_element(m.size(), "tag", &[]).unwrap();
    tags::delete_empty_tags(&mut m).unwrap();
    assert_eq!(
        "<tag>tag0</tag><tag>tag1</tag><tag>tag2</tag><tag>tag3</tag>",
        d.document().to_xml_string()
    );
}

#[test]
fn conversation_tags() {
    let mut view = view();
    assert!(view.root().unwrap().tags().is_empty());
    root(&mut view).add_tag("a").unwrap();
    root(&mut view).add_tag("b").unwrap();
    assert!(root(&mut view).remove_tag("a").unwrap());
    assert!(!root(&mut view).remove_tag("a").unwrap());
    assert_eq!(vec!["b"], view.root().unwrap().tags());
    assert_eq!("<tag>b</tag>", doc_xml(&view, "tags"));
}
