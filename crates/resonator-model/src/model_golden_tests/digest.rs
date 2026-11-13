//! Scenarios of the digest (`digestScenarios` in the generator).

use super::{Wave, alice, bob};
use crate::conversation::make_wavelet_conversational;
use crate::data::WaveletData;
use crate::digest::build_digest_with_supplement;
use crate::doc::XmlFragment;
use crate::doc::line_containers::append_line;
use crate::docop::Attributes;
use crate::id::ParticipantId;
use crate::supplement::{
    DefaultFollow, SupplementedWaveImpl, WaveletBasedSupplement, WaveletDataView,
};

const ROOT: &str = "conv+root";

fn digest(w: &mut Wave, viewer: &ParticipantId) {
    let answer = {
        let wave = w.convs.wave();
        let wavelets: Vec<&WaveletData> = wave.wavelets().map(|wavelet| wavelet.data()).collect();
        let d = build_digest_with_supplement(viewer, wave.wave_id(), &wavelets);
        format!(
            "digest title [{}] snippet [{}] participants [{}] modified {} created {} unread {} blips {}",
            d.title,
            d.snippet,
            d.participants.join(", "),
            d.last_modified,
            d.created,
            d.unread_count,
            d.blip_count
        )
    };
    w.q(answer);
}

/// Runs `change` on the viewer's supplemented wave.
fn with_supplement(
    w: &mut Wave,
    change: impl FnOnce(
        &mut SupplementedWaveImpl<WaveletBasedSupplement<'_>, WaveletDataView<'_>>,
    ) -> Result<(), crate::supplement::SupplementError>,
) -> super::StepResult {
    let viewer = w.convs.wave().viewer().clone();
    let conversations: Vec<WaveletData> = w
        .convs
        .wave()
        .wavelets()
        .map(|wavelet| wavelet.data().clone())
        .collect();
    let udw = w.wavelet(&format!("user+{}", viewer.address()));
    let view = WaveletDataView::new(viewer, conversations.iter());
    let mut supplemented = SupplementedWaveImpl::new(
        WaveletBasedSupplement::create(udw)?,
        view,
        DefaultFollow::Always,
    );
    Ok(change(&mut supplemented)?)
}

#[test]
fn digest_basic() {
    let mut wave = Wave::new(alice());
    wave.step("empty wave", |w| {
        digest(w, &alice());
        Ok(())
    });
    wave.step("non-conversational wavelet", |w| {
        w.edit(ROOT, "some-doc", |doc| doc.insert_text(0, "hello"))?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("clear", |w| {
        w.edit(ROOT, "some-doc", |doc| doc.delete_range(0, 5))
    });
    wave.step("empty conversation", |w| {
        make_wavelet_conversational(w.wavelet(ROOT))?;
        digest(w, &alice());
        Ok(())
    });
    wave.check("digest");
}

#[test]
fn digest_conversation() {
    let mut wave = Wave::new(alice());
    let mut id = vec![String::new(); 10];
    let root_id = Wave::wavelet_id(ROOT);
    wave.step("root blip", |w| {
        id[1] = w.convs.create_root()?.append_blip("")?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("text", |w| {
        w.convs.root_mut()?.edit_blip(&id[1], |doc| {
            doc.insert_text(3, "  The title.  Some body text ")
        })?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("implicit title", |w| {
        w.convs.root_mut()?.update_implicit_title()?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("more lines and blips", |w| {
        let mut c = w.convs.root_mut()?;
        c.edit_blip(&id[1], |doc| {
            append_line(
                doc,
                Some(&XmlFragment::text("Second line")),
                Some(&Attributes::new()),
            )
            .map(drop)
        })?;
        id[2] = c.append_blip("")?;
        c.edit_blip(&id[2], |doc| {
            doc.insert_text(3, "Second blip \u{e9}\u{4e16}")
        })?;
        let thread = c.add_inline_reply_thread(&id[1], 6)?;
        id[3] = c.append_blip(&thread)?;
        c.edit_blip(&id[3], |doc| doc.insert_text(3, "Inline reply"))?;
        c.add_participant(&bob())?;
        digest(w, &alice());
        digest(w, &bob());
        Ok(())
    });
    wave.step("edit old blip", |w| {
        w.convs
            .root_mut()?
            .edit_blip(&id[2], |doc| doc.insert_text(3, "Edited. "))?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("long text", |w| {
        let text: String = (0..30).map(|i| format!("word{i} ")).collect();
        w.convs
            .root_mut()?
            .edit_blip(&id[3], |doc| doc.insert_text(3, &text))?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("mark blip read", |w| {
        with_supplement(w, |s| s.mark_blip_as_read(&root_id, &id[1]))?;
        digest(w, &alice());
        digest(w, &bob());
        Ok(())
    });
    wave.step("edit read blip", |w| {
        w.convs
            .root_mut()?
            .edit_blip(&id[1], |doc| doc.insert_text(3, "Now "))?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("mark wave read", |w| {
        with_supplement(w, |s| s.mark_as_read())?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("new blip", |w| {
        id[4] = w.convs.root_mut()?.append_blip("")?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("explicit title", |w| {
        w.convs.root_mut()?.set_explicit_title("Now  The")?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("many participants", |w| {
        for name in ["c", "d", "e", "f", "g"] {
            let participant = ParticipantId::of(&format!("{name}@example.com"))?;
            w.convs.root_mut()?.add_participant(&participant)?;
        }
        digest(w, &alice());
        Ok(())
    });
    wave.step("mark unread", |w| {
        with_supplement(w, |s| s.mark_as_unread())?;
        digest(w, &alice());
        Ok(())
    });
    wave.step("delete blips", |w| {
        w.convs.root_mut()?.delete_blip(&id[2])?;
        w.convs.root_mut()?.delete_blip(&id[1])?;
        digest(w, &alice());
        Ok(())
    });
    wave.check("digest-conversation");
}

#[test]
fn digest_other_conversation() {
    let mut wave = Wave::new(alice());
    wave.step("non-root conversation", |w| {
        let mut c = w.convs.create_conversation()?;
        let blip = c.append_blip("")?;
        c.edit_blip(&blip, |doc| doc.insert_text(3, "Private reply"))?;
        // The original fails with a null pointer; the port gives a digest
        // without title and counts.
        let wave = w.convs.wave();
        let wavelets: Vec<&WaveletData> = wave.wavelets().map(|wavelet| wavelet.data()).collect();
        let d = build_digest_with_supplement(&alice(), wave.wave_id(), &wavelets);
        assert_eq!(
            ("", "Private reply", 0, 0, -1),
            (
                d.title.as_str(),
                d.snippet.as_str(),
                d.unread_count,
                d.blip_count,
                d.last_modified
            )
        );
        w.q("digest fails");
        Ok(())
    });
    wave.check("digest-other-conversation");
}
