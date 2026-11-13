//! Scenarios of the supplement (the `*Scenario` methods of the generator).
//!
//! The generator runs most scenarios twice, once keeping one
//! `WaveletBasedSupplement` for the whole scenario (`<name>/live`) and once
//! creating a new one in every step (`<name>/fresh`). The port has no
//! long-lived supplement, so one run here is compared with both.

use std::collections::BTreeMap;

use super::{OTHER, ROOT, StepResult, UDW, Wave, alice, bob, list, nullable, wid};
use crate::adt::java_double_to_string;
use crate::data::WaveletData;
use crate::supplement::{
    DefaultFollow, PrimitiveSupplement, PrimitiveSupplementImpl, ReadablePrimitiveSupplement,
    SupplementedWaveImpl, TRASH_FOLDER, ThreadState, WantedEvaluation, WaveletBasedSupplement,
    WaveletDataView,
};
use crate::version::HashedVersion;

type Sw<'a, 'b> = SupplementedWaveImpl<WaveletBasedSupplement<'a>, WaveletDataView<'b>>;

/// A wave with the supplement of its author (the generator's `Sup`).
struct Sup {
    wave: Wave,
    follow: DefaultFollow,
}

impl Sup {
    fn new(follow: DefaultFollow) -> Self {
        Sup {
            wave: Wave::new(),
            follow,
        }
    }

    /// A step that uses the supplement, which is created at its start.
    fn step(
        &mut self,
        label: &str,
        step: impl FnOnce(&mut Sw<'_, '_>, &mut Vec<String>) -> StepResult,
    ) {
        let follow = self.follow;
        self.wave.step(label, |w| {
            w.wavelet(UDW);
            let others: Vec<WaveletData> = w
                .view
                .wavelets()
                .map(|wavelet| wavelet.data().clone())
                .collect();
            let view = WaveletDataView::new(alice(), &others);
            let mut answers = Vec::new();
            let result = (|| {
                let primitive = WaveletBasedSupplement::create(w.wavelet(UDW))?;
                let mut supplemented = SupplementedWaveImpl::new(primitive, view, follow);
                step(&mut supplemented, &mut answers)
            })();
            for answer in answers {
                w.q(answer);
            }
            result
        });
    }

    /// A step that creates the supplement and then changes the wave.
    fn wave_step(&mut self, label: &str, step: impl FnOnce(&mut Wave) -> StepResult) {
        self.wave.step(label, |w| {
            WaveletBasedSupplement::create(w.wavelet(UDW))?;
            step(w)
        });
    }

    fn edit(&mut self, label: &str, wavelet: &str, blip: &str) {
        self.wave_step(label, |w| {
            Ok(w.edit(wavelet, blip, |doc| doc.insert_text(3, "x"))?)
        });
    }

    fn append(&mut self, label: &str, doc: &str, xml: &str) {
        self.wave_step(label, |w| w.append(UDW, doc, xml));
    }
}

/// Builds a conversation with three blips, the third in a reply thread.
fn conversation(w: &mut Wave, wavelet: &str) -> StepResult {
    w.append(
        wavelet,
        "conversation",
        "<conversation><blip id=\"b+1\"></blip>\
         <blip id=\"b+2\"><thread id=\"t+1\"><blip id=\"b+3\"></blip></thread></blip>\
         </conversation>",
    )?;
    for blip in ["b+1", "b+2", "b+3"] {
        w.append(wavelet, blip, "<body><line></line>text</body>")?;
    }
    Ok(())
}

fn ids(wavelets: Vec<crate::id::WaveletId>) -> String {
    let mut ids: Vec<String> = wavelets.iter().map(|id| id.id().to_owned()).collect();
    ids.sort();
    list(ids)
}

fn unread(sw: &Sw<'_, '_>, a: &mut Vec<String>, wavelet: &str) {
    let id = wid(wavelet);
    a.push(format!(
        "unread {wavelet} {} {} {} participants {} {} tags {}",
        sw.is_unread(&id, "b+1"),
        sw.is_unread(&id, "b+2"),
        sw.is_unread(&id, "b+3"),
        sw.is_participants_unread(&id),
        sw.have_participants_ever_been_read(&id),
        sw.is_tags_unread(&id),
    ));
    let prim = sw.supplement().primitive();
    a.push(format!(
        "read versions {wavelet} {} {} {} {} {} {} {} {}",
        prim.last_read_blip_version(&id, "b+1"),
        prim.last_read_blip_version(&id, "b+2"),
        prim.last_read_blip_version(&id, "b+3"),
        prim.last_read_participants_version(&id),
        prim.last_read_tags_version(&id),
        prim.last_read_wavelet_version(&id),
        list(prim.read_blips(&id)),
        ids(prim.read_wavelets()),
    ));
}

fn inbox(sw: &Sw<'_, '_>, a: &mut Vec<String>) {
    let prim = sw.supplement().primitive();
    a.push(format!(
        "inbox {} archived {} mute {} followed {} {} trashed {} folders {}",
        sw.is_inbox(),
        sw.is_archived(),
        sw.is_mute(),
        sw.is_followed(),
        nullable(prim.followed()),
        sw.is_trashed(),
        list(sw.folders()),
    ));
    a.push(format!(
        "archive versions {} {} {}",
        prim.archive_wavelet_version(&wid(ROOT)),
        prim.archive_wavelet_version(&wid(OTHER)),
        ids(prim.archive_wavelets()),
    ));
}

fn seen(sw: &Sw<'_, '_>, a: &mut Vec<String>) {
    let prim = sw.supplement().primitive();
    let (root, other) = (wid(ROOT), wid(OTHER));
    a.push(format!(
        "seen {} {} {} {} notified {} {} {} pending {} {} {} {}",
        sw.has_been_seen(),
        sw.seen_version(&root),
        sw.seen_version(&other),
        ids(prim.seen_wavelets()),
        prim.notified_version(&root),
        prim.notified_version(&other),
        ids(prim.notified_wavelets()),
        sw.has_pending_notification(),
        prim.pending_notification(),
        sw.supplement().has_pending_notification_for(&root),
        sw.supplement().has_pending_notification_for(&other),
    ));
}

#[test]
fn sup_read() {
    let mut s = Sup::new(DefaultFollow::Always);
    let root = wid(ROOT);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        w.wavelet(ROOT).add_participant(&alice())?;
        w.wavelet(ROOT).add_participant(&bob())?;
        Ok(())
    });
    let show = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            unread(sw, a, ROOT);
            Ok(())
        });
    };
    let read_blip = |s: &mut Sup, label: &str, blip: &str| {
        s.step(label, |sw, _| Ok(sw.mark_blip_as_read(&wid(ROOT), blip)?));
    };
    let read_participants = |s: &mut Sup, label: &str| {
        s.step(label, |sw, _| Ok(sw.mark_participants_as_read(&wid(ROOT))?));
    };
    let read_tags = |s: &mut Sup, label: &str| {
        s.step(label, |sw, _| Ok(sw.mark_tags_as_read(&wid(ROOT))?));
    };
    let read_wave = |s: &mut Sup, label: &str| {
        s.step(label, |sw, _| Ok(sw.mark_as_read()?));
    };
    show(&mut s, "initial");
    read_blip(&mut s, "read b+1", "b+1");
    show(&mut s, "after read b+1");
    read_blip(&mut s, "read b+1 again", "b+1");
    s.edit("edit b+1", ROOT, "b+1");
    show(&mut s, "after edit b+1");
    read_blip(&mut s, "read b+1 after edit", "b+1");
    read_blip(&mut s, "read b+2", "b+2");
    show(&mut s, "after read b+2");
    read_participants(&mut s, "read participants");
    read_tags(&mut s, "read tags");
    show(&mut s, "after read participants and tags");
    read_participants(&mut s, "read participants again");
    s.wave_step("remove bob", |w| {
        Ok(w.wavelet(ROOT).remove_participant(&bob())?)
    });
    show(&mut s, "after remove bob");
    read_participants(&mut s, "read participants after change");
    read_tags(&mut s, "read tags after change");
    read_wave(&mut s, "read wave");
    show(&mut s, "after read wave");
    read_wave(&mut s, "read wave again");
    read_blip(&mut s, "read b+3 when wave read", "b+3");
    s.edit("edit b+3", ROOT, "b+3");
    show(&mut s, "after edit b+3");
    read_blip(&mut s, "read b+3", "b+3");
    read_wave(&mut s, "read wave after edit");
    show(&mut s, "after read wave after edit");
    s.step("mark blip read at old version", |sw, _| {
        Ok(sw
            .supplement_mut()
            .mark_blip_as_read(&wid(ROOT), "b+2", 3)?)
    });
    s.step("mark blip read at future version", |sw, _| {
        Ok(sw
            .supplement_mut()
            .mark_blip_as_read(&wid(ROOT), "b+2", 1000)?)
    });
    s.step("clear blip read state", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.clear_blip_read_state(&wid(ROOT), "b+1")?)
    });
    show(&mut s, "after clear blip read state");
    s.step("unread", |sw, _| Ok(sw.mark_as_unread()?));
    show(&mut s, "after unread");
    s.step("unread again", |sw, _| Ok(sw.mark_as_unread()?));
    read_blip(&mut s, "read b+2 after unread", "b+2");
    show(&mut s, "at the end");
    s.step("copy", |sw, a| {
        let copy = PrimitiveSupplementImpl::copy_of(sw.supplement().primitive());
        a.push(format!(
            "{} {} {} {} {} {}",
            copy.last_read_blip_version(&root, "b+1"),
            copy.last_read_blip_version(&root, "b+2"),
            copy.last_read_participants_version(&root),
            copy.last_read_tags_version(&root),
            copy.last_read_wavelet_version(&root),
            ids(copy.read_wavelets()),
        ));
        Ok(())
    });
    s.wave.check("sup-read/live");
    s.wave.check("sup-read/fresh");
}

#[test]
fn sup_inbox() {
    let mut s = Sup::new(DefaultFollow::Always);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        w.wavelet(ROOT).add_participant(&alice())?;
        Ok(())
    });
    let show = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            inbox(sw, a);
            Ok(())
        });
    };
    let archive = |s: &mut Sup, label: &str| {
        s.step(label, |sw, _| Ok(sw.archive()?));
    };
    let folder = |s: &mut Sup, label: &str, folder: i32| {
        s.step(label, |sw, _| Ok(sw.move_to_folder(folder)?));
    };
    show(&mut s, "initial");
    archive(&mut s, "archive");
    show(&mut s, "after archive");
    archive(&mut s, "archive again");
    s.edit("edit", ROOT, "b+1");
    show(&mut s, "after edit");
    archive(&mut s, "archive after edit");
    show(&mut s, "after archive after edit");
    s.step("mute", |sw, _| Ok(sw.mute()?));
    show(&mut s, "after mute");
    s.step("mute again", |sw, _| Ok(sw.mute()?));
    archive(&mut s, "archive when muted");
    s.step("follow", |sw, _| Ok(sw.follow()?));
    show(&mut s, "after follow");
    archive(&mut s, "archive at same version after clear");
    show(&mut s, "after archive at same version");
    s.step("inbox", |sw, _| Ok(sw.inbox()?));
    show(&mut s, "after inbox");
    s.step("inbox again", |sw, _| Ok(sw.inbox()?));
    folder(&mut s, "trash", TRASH_FOLDER);
    show(&mut s, "after trash");
    folder(&mut s, "move to folder 5", 5);
    show(&mut s, "after move to folder 5");
    let add_folder = |s: &mut Sup, label: &str, folder: i32| {
        s.step(label, |sw, _| {
            Ok(sw.supplement_mut().primitive_mut().add_folder(folder)?)
        });
    };
    add_folder(&mut s, "add folder 12", 12);
    add_folder(&mut s, "add folder 12 again", 12);
    add_folder(&mut s, "add folder 2", 2);
    s.step("after add folders", |sw, a| {
        inbox(sw, a);
        let prim = sw.supplement().primitive();
        a.push(format!(
            "{} {}",
            prim.is_in_folder(12),
            prim.is_in_folder(13)
        ));
        Ok(())
    });
    for label in ["remove folder 5", "remove folder 5 again"] {
        s.step(label, |sw, _| {
            Ok(sw.supplement_mut().primitive_mut().remove_folder(5)?)
        });
    }
    folder(&mut s, "move to all folder", 3);
    show(&mut s, "after move to all folder");
    folder(&mut s, "move to folder 6", 6);
    folder(&mut s, "move to inbox folder", 1);
    show(&mut s, "after move to inbox folder");
    s.step("unfollow", |sw, _| Ok(sw.unfollow()?));
    s.step("clear follow", |sw, _| {
        Ok(sw.supplement_mut().primitive_mut().clear_follow()?)
    });
    show(&mut s, "after clear follow");
    s.step("clear follow again", |sw, _| {
        Ok(sw.supplement_mut().primitive_mut().clear_follow()?)
    });
    s.step("clear archive", |sw, _| {
        Ok(sw.supplement_mut().clear_archive()?)
    });
    s.step("archive at lower version", |sw, _| {
        Ok(sw.supplement_mut().archive(&wid(ROOT), 2)?)
    });
    show(&mut s, "after archive at lower version");
    s.wave.check("sup-inbox/live");
    s.wave.check("sup-inbox/fresh");
}

#[test]
fn sup_follow() {
    let mut s = Sup::new(DefaultFollow::WhenParticipant);
    s.wave.step("setup", |w| conversation(w, ROOT));
    let show = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            inbox(sw, a);
            Ok(())
        });
    };
    show(&mut s, "not a participant");
    s.step("archive when not followed", |sw, _| Ok(sw.archive()?));
    s.step("follow", |sw, _| Ok(sw.follow()?));
    show(&mut s, "after follow");
    s.step("clear follow", |sw, _| {
        Ok(sw.supplement_mut().primitive_mut().clear_follow()?)
    });
    s.wave_step("bob joins", |w| {
        Ok(w.wavelet(ROOT).add_participant(&bob())?)
    });
    show(&mut s, "after bob joins");
    s.wave_step("alice joins", |w| {
        Ok(w.wavelet(ROOT).add_participant(&alice())?)
    });
    show(&mut s, "after alice joins");
    s.step("unfollow", |sw, _| Ok(sw.unfollow()?));
    show(&mut s, "after unfollow");
    s.wave.check("sup-follow/live");
    s.wave.check("sup-follow/fresh");
}

#[test]
fn sup_multi() {
    let mut s = Sup::new(DefaultFollow::Always);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        conversation(w, OTHER)?;
        // Conversational by id, but without a manifest.
        w.append("conv+empty", "b+1", "<body><line></line></body>")?;
        w.append(
            "other+data",
            "conversation",
            "<conversation></conversation>",
        )?;
        w.wavelet(OTHER).add_participant(&alice())?;
        Ok(())
    });
    let show_unread = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            unread(sw, a, ROOT);
            unread(sw, a, OTHER);
            Ok(())
        });
    };
    let show_inbox = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            inbox(sw, a);
            Ok(())
        });
    };
    let show_seen = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            seen(sw, a);
            Ok(())
        });
    };
    s.step("initial", |sw, a| {
        unread(sw, a, ROOT);
        unread(sw, a, OTHER);
        inbox(sw, a);
        seen(sw, a);
        Ok(())
    });
    s.step("read wave", |sw, _| Ok(sw.mark_as_read()?));
    show_unread(&mut s, "after read wave");
    s.edit("edit other", OTHER, "b+2");
    show_unread(&mut s, "after edit other");
    s.step("read wave after edit", |sw, _| Ok(sw.mark_as_read()?));
    s.step("archive", |sw, _| Ok(sw.archive()?));
    show_inbox(&mut s, "after archive");
    s.edit("edit root", ROOT, "b+1");
    show_inbox(&mut s, "after edit root");
    s.step("archive after edit", |sw, _| Ok(sw.archive()?));
    show_inbox(&mut s, "after archive after edit");
    s.step("see at version zero", |sw, _| Ok(sw.see()?));
    show_seen(&mut s, "after see at version zero");
    s.wave_step("sign", |w| {
        w.wavelet(ROOT)
            .data_mut()
            .set_hashed_version(HashedVersion::of(40, vec![1, 2, 3]));
        w.wavelet(OTHER)
            .data_mut()
            .set_hashed_version(HashedVersion::of(41, vec![0xff, 0, 7, 9]));
        Ok(())
    });
    s.step("see root", |sw, _| Ok(sw.see_wavelet(&wid(ROOT))?));
    show_seen(&mut s, "after see root");
    s.step("see", |sw, _| Ok(sw.see()?));
    show_seen(&mut s, "after see");
    s.step("see again", |sw, _| Ok(sw.see()?));
    s.step("notify", |sw, _| Ok(sw.mark_as_notified()?));
    show_seen(&mut s, "after notify");
    s.wave_step("sign later", |w| {
        w.wavelet(OTHER)
            .data_mut()
            .set_hashed_version(HashedVersion::of(90, vec![5]));
        Ok(())
    });
    s.step("see other", |sw, _| Ok(sw.see_wavelet(&wid(OTHER))?));
    show_seen(&mut s, "after see other");
    for (label, version) in [("notified at 200", 200), ("notified at 100", 100)] {
        s.step(label, |sw, _| {
            let prim = sw.supplement_mut().primitive_mut();
            Ok(prim.set_notified_version(&wid(OTHER), version)?)
        });
    }
    show_seen(&mut s, "after notified");
    let seen_low = |s: &mut Sup, label: &str| {
        s.step(label, |sw, _| {
            let prim = sw.supplement_mut().primitive_mut();
            Ok(prim.set_seen_version(&wid(OTHER), &HashedVersion::of(3, vec![9]))?)
        });
    };
    seen_low(&mut s, "seen at lower version");
    s.step("clear seen other", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.clear_seen_version(&wid(OTHER))?)
    });
    show_seen(&mut s, "after clear seen other");
    seen_low(&mut s, "seen at lower version after clear");
    show_seen(&mut s, "at the end");
    s.step("unread", |sw, _| Ok(sw.mark_as_unread()?));
    s.wave.check("sup-multi/live");
    s.wave.check("sup-multi/fresh");
}

fn evaluation(
    wavelet: &str,
    wanted: bool,
    certainty: f64,
    timestamp: i64,
    agent: &str,
    ignored: bool,
) -> WantedEvaluation {
    WantedEvaluation {
        wavelet_id: Some(wid(wavelet)),
        adder_address: "bob@example.com".to_owned(),
        wanted,
        certainty,
        timestamp,
        agent_identity: agent.to_owned(),
        ignored,
        comment: "a \"comment\"".to_owned(),
    }
}

fn describe(e: &WantedEvaluation) -> String {
    format!(
        "{}/{}/{}/{}/{}/{}/{}/{}/{}",
        e.wavelet_id.as_ref().unwrap().id(),
        e.adder_address,
        e.wanted,
        java_double_to_string(e.certainty),
        e.timestamp,
        e.agent_identity,
        e.ignored,
        e.comment,
        e.java_hash_code(),
    )
}

fn wanted(sw: &Sw<'_, '_>, a: &mut Vec<String>, wavelet: &str) {
    let set = sw.wanted_evaluation_set(&wid(wavelet));
    let mut all: Vec<String> = set.evaluations().iter().map(describe).collect();
    all.sort();
    a.push(format!(
        "wanted {wavelet} {} {} {} {} {}",
        set.is_wanted(),
        set.is_ignored(),
        describe(&set.most_certain()),
        describe(&set.most_recent()),
        list(all),
    ));
}

/// Java's `TreeMap.toString`.
fn gadget(state: BTreeMap<String, String>) -> String {
    let entries: Vec<String> = state.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{{{}}}", entries.join(", "))
}

fn state_name(state: Option<ThreadState>) -> String {
    nullable(state.map(ThreadState::name))
}

#[test]
fn sup_misc() {
    let mut s = Sup::new(DefaultFollow::Always);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        // An empty wavelet, to ask for the evaluations of.
        w.wavelet(OTHER);
        Ok(())
    });
    let (root, other) = (wid(ROOT), wid(OTHER));

    s.step("no thread state", |sw, a| {
        let prim = sw.supplement().primitive();
        a.push(format!(
            "{} {} {}",
            state_name(prim.thread_state(&root, "t+1")),
            list(prim.stateful_threads(&root)),
            ids(prim.wavelets_with_thread_state()),
        ));
        Ok(())
    });
    let thread = |s: &mut Sup, label: &str, wavelet: &str, id: &str, state| {
        s.step(label, |sw, _| {
            let prim = sw.supplement_mut().primitive_mut();
            Ok(prim.set_thread_state(&wid(wavelet), id, state)?)
        });
    };
    let (collapsed, expanded) = (Some(ThreadState::Collapsed), Some(ThreadState::Expanded));
    thread(
        &mut s,
        "remove thread state of unknown wavelet",
        ROOT,
        "t+1",
        None,
    );
    s.step("collapse t+1", |sw, a| {
        sw.set_thread_state(&root, "t+1", collapsed)?;
        a.push(state_name(sw.thread_state(&root, "t+1")));
        Ok(())
    });
    thread(&mut s, "collapse t+1 again", ROOT, "t+1", collapsed);
    thread(&mut s, "expand t+1", ROOT, "t+1", expanded);
    thread(&mut s, "collapse t+2", ROOT, "t+2", collapsed);
    thread(&mut s, "collapse in other", OTHER, "t+9", collapsed);
    s.step("thread states", |sw, a| {
        let prim = sw.supplement().primitive();
        a.push(format!(
            "{} {} {} {} {} {} {}",
            state_name(prim.thread_state(&root, "t+1")),
            state_name(prim.thread_state(&root, "t+2")),
            state_name(prim.thread_state(&root, "t+3")),
            state_name(prim.thread_state(&other, "t+9")),
            list(prim.stateful_threads(&root)),
            list(prim.stateful_threads(&other)),
            ids(prim.wavelets_with_thread_state()),
        ));
        Ok(())
    });
    thread(&mut s, "remove t+1", ROOT, "t+1", None);
    thread(&mut s, "remove t+1 again", ROOT, "t+1", None);
    s.step("thread states after remove", |sw, a| {
        let prim = sw.supplement().primitive();
        a.push(format!(
            "{} {}",
            state_name(prim.thread_state(&root, "t+1")),
            list(prim.stateful_threads(&root)),
        ));
        Ok(())
    });

    s.step("no gadget state", |sw, a| {
        a.push(format!(
            "{} {}",
            gadget(sw.gadget_state("g1")),
            nullable(sw.gadget_state_value("g1", "k")),
        ));
        Ok(())
    });
    let set_gadget = |s: &mut Sup, label: &str, id: &str, key: &str, value: Option<&str>| {
        s.step(label, |sw, _| Ok(sw.set_gadget_state(id, key, value)?));
    };
    set_gadget(&mut s, "gadget remove on unknown gadget", "g0", "k", None);
    set_gadget(&mut s, "gadget set", "g1", "k", Some("v"));
    set_gadget(&mut s, "gadget set same", "g1", "k", Some("v"));
    set_gadget(
        &mut s,
        "gadget set other key",
        "g1",
        "k2",
        Some("v <2> & \"more\""),
    );
    set_gadget(&mut s, "gadget replace", "g1", "k", Some("w"));
    set_gadget(&mut s, "gadget in other", "g 2", "k", Some("x"));
    s.step("gadget states", |sw, a| {
        a.push(format!(
            "{} {} {} {} {}",
            gadget(sw.gadget_state("g0")),
            gadget(sw.gadget_state("g1")),
            gadget(sw.gadget_state("g 2")),
            nullable(sw.gadget_state_value("g1", "k2")),
            nullable(sw.gadget_state_value("g1", "zz")),
        ));
        Ok(())
    });
    set_gadget(&mut s, "gadget remove", "g1", "k", None);
    s.step("gadget states after remove", |sw, a| {
        a.push(gadget(sw.gadget_state("g1")));
        Ok(())
    });

    s.step("no evaluations", |sw, a| {
        wanted(sw, a, ROOT);
        Ok(())
    });
    let add = |s: &mut Sup, label: &str, evaluation: WantedEvaluation| {
        s.step(label, |sw, _| Ok(sw.add_wanted_evaluation(&evaluation)?));
    };
    add(
        &mut s,
        "evaluation 1",
        evaluation(ROOT, true, 0.5, 1000, "agent1", false),
    );
    add(
        &mut s,
        "evaluation 2",
        evaluation(ROOT, false, 0.75, 900, "agent2", true),
    );
    s.step("evaluations", |sw, a| {
        wanted(sw, a, ROOT);
        Ok(())
    });
    for label in ["evaluation 3", "evaluation 3 again"] {
        add(
            &mut s,
            label,
            evaluation(ROOT, true, 1.0E-4, 2000, "agent3", false),
        );
    }
    add(
        &mut s,
        "evaluation in other",
        evaluation(OTHER, false, 1.0E7, 1234567890123, "agent1", false),
    );
    add(
        &mut s,
        "evaluation 4",
        evaluation(ROOT, true, 0.75, 900, "agent4", false),
    );
    s.step("more evaluations", |sw, a| {
        wanted(sw, a, ROOT);
        wanted(sw, a, OTHER);
        let prim = sw.supplement().primitive();
        a.push(prim.wanted_evaluations().len().to_string());
        Ok(())
    });
    s.step("odd certainties", |sw, _| {
        let certainties = [
            0.0,
            1.0,
            100.0,
            1234567.125,
            12345678.5,
            0.001,
            0.00099,
            1e21,
            1e-7,
            0.1 + 0.2,
            123456789012345680.0,
            4.9e-324,
            1.7976931348623157e308,
            0.5e-3,
            9999999.999999998,
            1.0E-5,
            3.0E10,
        ];
        for certainty in certainties {
            sw.add_wanted_evaluation(&evaluation(OTHER, true, certainty, 1, "odd", false))?;
        }
        sw.add_wanted_evaluation(&evaluation(OTHER, true, -2.5, -7, "odd", false))?;
        Ok(())
    });
    s.step("all evaluations", |sw, a| {
        wanted(sw, a, OTHER);
        Ok(())
    });
    s.wave.check("sup-misc/live");
    s.wave.check("sup-misc/fresh");
}

#[test]
fn sup_corrupt() {
    let mut s = Sup::new(DefaultFollow::Always);
    let root = wid(ROOT);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        w.wavelet(ROOT).add_participant(&alice())?;
        Ok(())
    });
    let show_unread = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            unread(sw, a, ROOT);
            Ok(())
        });
    };
    let show_inbox = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            inbox(sw, a);
            Ok(())
        });
    };
    let show_seen = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            seen(sw, a);
            Ok(())
        });
    };
    s.step("empty", |sw, a| {
        unread(sw, a, ROOT);
        inbox(sw, a);
        seen(sw, a);
        Ok(())
    });
    s.append(
        "corrupt read state",
        "m/read",
        "<wavelet i=\"example.com/conv+root\"><all v=\"5\"></all><participants v=\"3\">\
         </participants><blip i=\"b+1\" v=\"2\"></blip><all v=\"9\"></all>\
         <blip i=\"b+1\" v=\"7\"></blip><blip i=\"b+1\" v=\"4\"></blip>\
         <blip i=\"b+2\" v=\"1\"></blip><all v=\"7\"></all><tags v=\"1\"></tags>\
         <tags v=\"1\"></tags><junk></junk><blip i=\"b+9\" v=\"7\"></blip></wavelet>\
         <wavelet i=\"example.com/conv+root\"><all v=\"100\"></all>\
         <blip i=\"b+3\" v=\"50\"></blip></wavelet><wavelet></wavelet><other></other>\
         <wavelet i=\"example.com/conv+other\"><participants v=\"8\"></participants>\
         <participants v=\"2\"></participants></wavelet>",
    );
    s.step("corrupt read state read", |sw, a| {
        unread(sw, a, ROOT);
        let prim = sw.supplement().primitive();
        a.push(format!(
            "{} {}",
            prim.last_read_participants_version(&wid(OTHER)),
            prim.last_read_blip_version(&root, "b+9"),
        ));
        Ok(())
    });
    let read_blip = |s: &mut Sup, label: &str, blip: &str, version: i32| {
        s.step(label, |sw, _| {
            let prim = sw.supplement_mut().primitive_mut();
            Ok(prim.set_last_read_blip_version(&wid(ROOT), blip, version)?)
        });
    };
    read_blip(&mut s, "read blip at lower version", "b+1", 6);
    read_blip(&mut s, "~read blip cleans up", "b+2", 5);
    s.step("~read wavelet cleans up", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.set_last_read_wavelet_version(&wid(ROOT), 12)?)
    });
    s.step("~read tags cleans up", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.set_last_read_tags_version(&wid(ROOT), 2)?)
    });
    s.step("~read participants of other cleans up", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.set_last_read_participants_version(&wid(OTHER), 9)?)
    });
    show_unread(&mut s, "after cleaning up");

    s.append(
        "corrupt muted",
        "m/muted",
        "<muted muted=\"false\"></muted><x></x><muted muted=\"true\"></muted><muted></muted>",
    );
    show_inbox(&mut s, "corrupt muted read");
    s.step("unfollow when muted", |sw, _| Ok(sw.unfollow()?));
    s.step("follow cleans up", |sw, _| Ok(sw.follow()?));
    s.append(
        "corrupt muted again",
        "m/muted",
        "<muted muted=\"TRUE\"></muted><muted muted=\"false\"></muted>",
    );
    s.step("clear follow cleans up", |sw, _| {
        Ok(sw.supplement_mut().primitive_mut().clear_follow()?)
    });
    show_inbox(&mut s, "after clear follow");

    s.wave_step("corrupt archive", |w| {
        w.append(
            UDW,
            "m/archiving",
            "<archive i=\"example.com/conv+root\" v=\"3\"></archive>\
             <archive i=\"example.com/conv+root\" v=\"8\"></archive>\
             <archive i=\"example.com/conv+root\" v=\"8\"></archive>",
        )?;
        w.append(
            UDW,
            "m/cleared",
            "<cleared cleared=\"true\"></cleared>\
             <cleared cleared=\"false\"></cleared><cleared cleared=\"true\"></cleared>",
        )
    });
    show_inbox(&mut s, "corrupt archive read");
    let archive = |s: &mut Sup, label: &str, version: i32| {
        s.step(label, |sw, _| {
            Ok(sw.supplement_mut().archive(&wid(ROOT), version)?)
        });
    };
    archive(&mut s, "archive when cleared", 5);
    show_inbox(&mut s, "after archive when cleared");
    archive(&mut s, "~archive cleans up", 30);
    show_inbox(&mut s, "after archive");

    s.append(
        "corrupt seen",
        "m/seen",
        "<seen i=\"example.com/conv+root\" signature=\"5:AQI=\"></seen>\
         <seen i=\"example.com/conv+root\" signature=\"5:AQM=\"></seen>\
         <seen i=\"example.com/conv+other\" signature=\"7\"></seen>\
         <notified i=\"example.com/conv+root\" v=\"4\"></notified>\
         <notified i=\"example.com/conv+root\" v=\"2\"></notified>\
         <notification pending=\"true\"></notification>\
         <notification pending=\"true\"></notification>",
    );
    show_seen(&mut s, "corrupt seen read");
    s.step("see cleans up", |sw, _| Ok(sw.see()?));
    show_seen(&mut s, "after see");

    s.append(
        "corrupt presentation",
        "m/presentation",
        "<conversation i=\"example.com/conv+root\"><thread i=\"t+1\" state=\"COLLAPSED\">\
         </thread><thread i=\"t+2\" state=\"EXPANDED\"></thread>\
         <thread i=\"t+1\" state=\"EXPANDED\"></thread></conversation>\
         <conversation i=\"example.com/conv+root\"><thread i=\"t+3\" state=\"COLLAPSED\">\
         </thread></conversation>",
    );
    s.step("corrupt presentation read", |sw, a| {
        let prim = sw.supplement().primitive();
        a.push(format!(
            "{} {} {} {}",
            state_name(prim.thread_state(&root, "t+1")),
            state_name(prim.thread_state(&root, "t+2")),
            state_name(prim.thread_state(&root, "t+3")),
            list(prim.stateful_threads(&root)),
        ));
        Ok(())
    });
    s.step("~thread state cleans up", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.set_thread_state(&wid(ROOT), "t+2", Some(ThreadState::Collapsed))?)
    });
    s.step("remove thread state", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.set_thread_state(&wid(ROOT), "t+1", None)?)
    });

    s.append(
        "corrupt gadgets",
        "m/gadgets",
        "<gadget i=\"g1\"><state name=\"a\" value=\"1\"></state><state name=\"b\" value=\"2\">\
         </state><state name=\"a\" value=\"3\"></state></gadget><gadget></gadget>\
         <gadget i=\"g1\"><state name=\"c\" value=\"4\"></state></gadget>",
    );
    let show_gadget = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            a.push(gadget(sw.supplement().primitive().gadget_state("g1")));
            Ok(())
        });
    };
    show_gadget(&mut s, "corrupt gadgets read");
    s.step("gadget remove cleans up", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        Ok(prim.set_gadget_state("g1", "zz", None)?)
    });
    show_gadget(&mut s, "after gadget remove");

    s.append(
        "corrupt abuse",
        "m/abuse",
        "<wanted wavelet_id=\"example.com/conv+root\"></wanted><x></x>\
         <wanted wavelet_id=\"example.com/conv+root\" wanted=\"TRUE\" certainty=\" 1e1 \" \
         timestamp=\"+12\" ignored=\"yes\" adder=\"a\" agent=\"b\" comment=\"c\"></wanted>\
         <wanted wavelet_id=\"example.com/conv+root\"></wanted>",
    );
    s.step("corrupt abuse read", |sw, a| {
        wanted(sw, a, ROOT);
        Ok(())
    });
    s.wave.check("sup-corrupt/live");
    s.wave.check("sup-corrupt/fresh");
}

#[test]
fn sup_legacy_folders() {
    let mut s = Sup::new(DefaultFollow::Always);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        w.append(
            UDW,
            "m/folder",
            "<folder i=\"5\"></folder><folder i=\"1\"></folder>\
             <x></x><folder i=\"5\"></folder><folder i=\"3\"></folder><folder i=\"9\"></folder>",
        )
    });
    let show = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            inbox(sw, a);
            Ok(())
        });
    };
    show(&mut s, "create removes legacy folders");
    s.step("add legacy folder", |sw, a| {
        let prim = sw.supplement_mut().primitive_mut();
        prim.add_folder(3)?;
        a.push(list(prim.folders()));
        Ok(())
    });
    show(&mut s, "next create removes it");
    s.step("add folders", |sw, _| {
        let prim = sw.supplement_mut().primitive_mut();
        prim.add_folder(2)?;
        prim.add_folder(15)?;
        prim.add_folder(0)?;
        Ok(())
    });
    for label in ["remove all folders", "remove all folders again"] {
        s.step(label, |sw, _| {
            Ok(sw.supplement_mut().primitive_mut().remove_all_folders()?)
        });
    }
    s.wave.check("sup-legacy-folders");
}

#[test]
fn sup_clear_duplicates() {
    let mut s = Sup::new(DefaultFollow::Always);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        w.append(
            UDW,
            "m/read",
            "<wavelet i=\"example.com/conv+root\"><all v=\"1\"></all></wavelet>\
             <wavelet i=\"example.com/conv+other\"><all v=\"2\"></all></wavelet>\
             <wavelet i=\"example.com/conv+root\"><all v=\"100\"></all>\
             <blip i=\"b+3\" v=\"50\"></blip></wavelet>",
        )
    });
    let show = |s: &mut Sup, label: &str| {
        s.step(label, |sw, a| {
            unread(sw, a, ROOT);
            Ok(())
        });
    };
    show(&mut s, "before");
    s.step("~unread", |sw, _| Ok(sw.mark_as_unread()?));
    show(&mut s, "after");
    s.step("unread again", |sw, _| Ok(sw.mark_as_unread()?));
    show(&mut s, "at the end");
    s.wave.check("sup-clear-duplicates");
}

#[test]
fn sup_udw_participants() {
    let mut s = Sup::new(DefaultFollow::Always);
    s.wave.step("setup", |w| {
        conversation(w, ROOT)?;
        w.wavelet(UDW).add_participant(&bob())?;
        Ok(())
    });
    s.step("follow", |sw, _| Ok(sw.follow()?));
    s.step("unfollow", |sw, _| Ok(sw.unfollow()?));
    s.wave.check("sup-udw-participants");
}
