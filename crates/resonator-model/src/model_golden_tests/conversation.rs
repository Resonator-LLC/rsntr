//! Scenarios of the conversation model and the op-based wavelet
//! (`conversationScenarios` in the generator).

use super::{StepResult, Wave, alice, bob};
use crate::conversation::{Conversation, ConversationThread, WaveletBasedConversation, title};
use crate::doc::XmlFragment;

fn dump(conversation: &Conversation<'_>, thread: &ConversationThread) -> String {
    let blips: Vec<String> = thread
        .blips()
        .iter()
        .map(|blip| {
            let replies: Vec<String> = conversation
                .locate_reply_threads(blip)
                .iter()
                .map(|reply| {
                    let location = reply.location.map_or(-1, |l| l as i64);
                    format!("{location}:{}", dump(conversation, reply.thread))
                })
                .collect();
            let root = if conversation.is_root_blip(blip.id()) {
                "^"
            } else {
                ""
            };
            format!("{}{root}{{{}}}", blip.id(), replies.join(","))
        })
        .collect();
    format!("T({})[{}]", thread.id(), blips.join(","))
}

fn query_conversation(w: &mut Wave, wavelet: &str) {
    let id = Wave::wavelet_id(wavelet);
    let answers = {
        let conversation = w.convs.conversation(&id).unwrap().unwrap();
        let mut order = "bfs".to_owned();
        for blip in conversation.blips_breadth_first() {
            order.push(' ');
            order.push_str(blip.id());
        }
        vec![
            format!(
                "structure {}",
                dump(&conversation, conversation.root_thread())
            ),
            order,
            format!("title {}", conversation.title()),
        ]
    };
    for answer in answers {
        w.q(answer);
    }
}

fn query_blip(w: &mut Wave, wavelet: &str, blip_id: &str) {
    let id = Wave::wavelet_id(wavelet);
    let answer = {
        let conversation = w.convs.conversation(&id).unwrap().unwrap();
        let blip = conversation.blip_data(blip_id).unwrap();
        let mut contributors: Vec<&str> = blip.contributors().iter().map(|p| p.address()).collect();
        contributors.sort();
        format!(
            "blip {} author {} contributors [{}] lmv {} lmt {}",
            blip.id(),
            blip.author(),
            contributors.join(", "),
            blip.last_modified_version(),
            blip.last_modified_time()
        )
    };
    w.q(answer);
}

const ROOT: &str = "conv+root";

fn root<'a>(w: &'a mut Wave) -> WaveletBasedConversation<'a> {
    w.convs.root_mut().unwrap()
}

#[test]
fn conv_basic() {
    let mut wave = Wave::new(alice());
    wave.step("create root", |w| Ok(w.convs.create_root().map(drop)?));
    wave.step("query empty", |w| {
        let (id, view) = (w.convs.root().unwrap().id(), w.convs.id());
        w.q(format!("id {id}"));
        w.q(format!("view {view}"));
        query_conversation(w, ROOT);
        Ok(())
    });
    let mut id = vec![String::new(); 10];
    let mut thread = vec![String::new(); 10];
    wave.step("append root blip", |w| {
        id[1] = root(w).append_blip("")?;
        Ok(())
    });
    wave.step("write text", |w| {
        Ok(root(w).edit_blip(&id[1], |doc| doc.insert_text(3, "Hello world. More text"))?)
    });
    wave.step("implicit title", |w| {
        root(w).update_implicit_title()?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.step("implicit title again", |w| {
        Ok(root(w).update_implicit_title()?)
    });
    wave.step("append second blip", |w| {
        id[2] = root(w).append_blip("")?;
        Ok(())
    });
    wave.step("reply thread", |w| {
        thread[1] = root(w).add_reply_thread(&id[1])?;
        Ok(())
    });
    wave.step("reply blip", |w| {
        id[3] = root(w).append_blip(&thread[1])?;
        Ok(())
    });
    wave.step("inline reply thread", |w| {
        thread[2] = root(w).add_inline_reply_thread(&id[1], 8)?;
        Ok(())
    });
    wave.step("inline reply blips", |w| {
        id[4] = root(w).append_blip(&thread[2])?;
        id[5] = root(w).append_blip(&thread[2])?;
        Ok(())
    });
    wave.step("second inline reply thread, earlier", |w| {
        thread[3] = root(w).add_inline_reply_thread(&id[1], 5)?;
        id[6] = root(w).append_blip(&thread[3])?;
        Ok(())
    });
    wave.step("nested reply", |w| {
        thread[4] = root(w).add_reply_thread(&id[3])?;
        id[7] = root(w).append_blip(&thread[4])?;
        Ok(())
    });
    wave.step("insert blip", |w| {
        id[8] = root(w).insert_blip("", &id[2])?;
        Ok(())
    });
    wave.step("query", |w| {
        query_conversation(w, ROOT);
        for blip_id in &id[1..=8] {
            query_blip(w, ROOT, blip_id);
        }
        Ok(())
    });
    wave.step("explicit title", |w| {
        root(w).set_explicit_title("Explicit")?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.step("implicit title with explicit present", |w| {
        Ok(root(w).update_implicit_title()?)
    });
    wave.step("participants", |w| {
        root(w).add_participant(&bob())?;
        root(w).add_participant(&bob())?;
        root(w).remove_participant(&alice())?;
        root(w).remove_participant(&alice())?;
        Ok(())
    });
    wave.step("author is no participant", |w| {
        Ok(root(w).edit_blip(&id[2], |doc| doc.insert_text(3, "x"))?)
    });
    wave.step("delete first inline blip", |w| {
        Ok(root(w).delete_blip(&id[4])?)
    });
    wave.step("delete last inline blip", |w| {
        root(w).delete_blip(&id[5])?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.step("delete nested thread", |w| {
        Ok(root(w).delete_thread(&thread[4])?)
    });
    wave.step("delete reply thread", |w| {
        Ok(root(w).delete_thread(&thread[1])?)
    });
    wave.step("delete inserted blip", |w| Ok(root(w).delete_blip(&id[8])?));
    wave.step("delete blip with inline reply", |w| {
        root(w).delete_blip(&id[1])?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.step("tags", |w| {
        let mut c = root(w);
        c.add_tag("one")?;
        c.add_tag("two & <three>")?;
        c.add_tag("four")?;
        c.remove_tag("two & <three>")?;
        c.remove_tag("missing")?;
        c.edit_data_document("tags", |doc| {
            crate::conversation::tags::insert_tag(doc, "zero", 0)
        })?;
        Ok(())
    });
    wave.step("delete conversation", |w| Ok(root(w).delete()?));
    wave.check("conv-basic");
}

#[test]
fn conv_delete_many() {
    let mut wave = Wave::new(alice());
    let mut id = vec![String::new(); 10];
    let mut thread = vec![String::new(); 10];
    wave.step("build", |w| {
        let mut c = w.convs.create_root()?;
        id[1] = c.append_blip("")?;
        c.edit_blip(&id[1], |doc| doc.insert_text(3, "Some text here"))?;
        thread[1] = c.add_inline_reply_thread(&id[1], 5)?;
        id[2] = c.append_blip(&thread[1])?;
        id[3] = c.append_blip(&thread[1])?;
        thread[2] = c.add_reply_thread(&id[1])?;
        id[4] = c.append_blip(&thread[2])?;
        thread[3] = c.add_inline_reply_thread(&id[1], 9)?;
        id[5] = c.append_blip(&thread[3])?;
        thread[4] = c.add_inline_reply_thread(&id[3], 4)?;
        id[6] = c.append_blip(&thread[4])?;
        id[7] = c.append_blip("")?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.step_unordered("delete blip with many replies", |w| {
        root(w).delete_blip(&id[1])?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.step_unordered("delete root thread", |w| {
        root(w).delete_thread("")?;
        query_conversation(w, ROOT);
        Ok(())
    });
    wave.check("conv-delete-many");
}

#[test]
fn conv_anchor() {
    let mut wave = Wave::new(alice());
    let mut id = vec![String::new(); 10];
    let root_id = Wave::wavelet_id(ROOT);
    let mut other = None;
    wave.step("create root", |w| {
        id[1] = w.convs.create_root()?.append_blip("")?;
        Ok(())
    });
    let has_anchor = |w: &mut Wave, other: &crate::id::WaveletId| {
        let has = w.convs.anchor_of(other).is_some();
        w.q(format!("has anchor {has}"));
    };
    wave.step("create conversation", |w| {
        let mut c = w.convs.create_conversation()?;
        id[2] = c.append_blip("")?;
        let wavelet_id = c.wavelet().data().wavelet_id().clone();
        let conversation_id = c.read()?.id();
        w.q(format!("id {conversation_id}"));
        has_anchor(w, &wavelet_id);
        other = Some(wavelet_id);
        Ok(())
    });
    let other = other.unwrap();
    wave.step("set anchor", |w| {
        w.convs.set_anchor(&other, Some((&root_id, &id[1])))?;
        has_anchor(w, &other);
        let (wavelet, blip) = w.convs.anchor_of(&other).unwrap();
        let conversation_id = crate::conversation::conversation_id_for(&wavelet);
        w.q(format!("anchor {conversation_id} {blip}"));
        Ok(())
    });
    wave.step("set same anchor", |w| {
        Ok(w.convs.set_anchor(&other, Some((&root_id, &id[1])))?)
    });
    wave.step("delete anchor blip", |w| {
        root(w).delete_blip(&id[1])?;
        has_anchor(w, &other);
        Ok(())
    });
    wave.step("clear anchor", |w| {
        w.convs.set_anchor(&other, None)?;
        has_anchor(w, &other);
        Ok(())
    });
    wave.step("append with content", |w| {
        w.convs
            .conversation_mut(&other)?
            .append_blip_with_content("", Some(&title::empty_document_with_title()))?;
        Ok(())
    });
    wave.check("conv-anchor");
}

#[test]
fn wavelet_authorise() {
    let mut wave = Wave::new(bob());
    fn step(
        wave: &mut Wave,
        label: &str,
        f: impl FnOnce(&mut crate::wavelet::OpBasedWavelet) -> StepResult,
    ) {
        wave.step(label, |w| f(w.wavelet(ROOT)));
    }
    step(&mut wave, "add alice to empty wavelet", |w| {
        Ok(w.add_participant(&alice())?)
    });
    step(&mut wave, "edit as non-participant", |w| {
        let body = XmlFragment::empty().wrap("line").wrap("body");
        Ok(w.edit_document("b+x", |doc| doc.insert_xml(doc.size(), &body).map(drop))?)
    });
    step(&mut wave, "remove bob", |w| {
        Ok(w.remove_participant(&bob())?)
    });
    step(&mut wave, "add self as non-participant", |w| {
        Ok(w.add_participant(&bob())?)
    });
    step(&mut wave, "remove bob again", |w| {
        Ok(w.remove_participant(&bob())?)
    });
    step(&mut wave, "remove alice as non-participant", |w| {
        Ok(w.remove_participant(&alice())?)
    });
    step(&mut wave, "touch empty wavelet", |w| Ok(w.touch()?));
    wave.check("wavelet-authorise");
}
