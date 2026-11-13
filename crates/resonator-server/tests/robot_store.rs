//! The robot registrations, agent triggers and usage of schema migrations 5
//! and 6.

mod common;

use common::{TestDb, participant};
use resonator_server::storage::{AgentUsage, RobotRegistration, TriggerState};

#[tokio::test]
async fn registrations_round_trip_and_survive_reopening() {
    let test = TestDb::new();
    let db = test.open();
    let store = db.robot_store();
    let robot = participant("bot@example.com");
    let owner = participant("alice@example.com");
    store
        .put_registration(RobotRegistration {
            robot: robot.clone(),
            owner: owner.clone(),
            webhook_secret: "s1".to_owned(),
            created_at: 5,
        })
        .await
        .unwrap();
    // Replacing keeps the creation time, takes the new secret.
    store
        .put_registration(RobotRegistration {
            robot: robot.clone(),
            owner: owner.clone(),
            webhook_secret: "s2".to_owned(),
            created_at: 9,
        })
        .await
        .unwrap();
    let found = store.registration(&robot).await.unwrap().unwrap();
    assert_eq!((found.webhook_secret.as_str(), found.created_at), ("s2", 5));
    // The secret never shows in debug output.
    assert!(!format!("{found:?}").contains("s2"));
    db.close().await;

    let db = test.open();
    let store = db.robot_store();
    assert_eq!(store.registrations(Some(&owner)).await.unwrap().len(), 1);
    assert!(store.remove_registration(&robot).await.unwrap());
    assert!(!store.remove_registration(&robot).await.unwrap());
    assert!(store.registration(&robot).await.unwrap().is_none());
    db.close().await;
}

#[tokio::test]
async fn triggers_are_answered_exactly_once() {
    let test = TestDb::new();
    let db = test.open();
    let store = db.robot_store();
    let agent = participant("helper@example.com");
    let wave = "example.com/w+a/~/example.com/conv+root";
    assert_eq!(
        store.note_trigger(&agent, wave, "b+1", 1).await.unwrap(),
        TriggerState::Pending
    );
    // Noting again changes nothing.
    assert_eq!(
        store.note_trigger(&agent, wave, "b+1", 2).await.unwrap(),
        TriggerState::Pending
    );
    assert!(
        store
            .advance_trigger(
                &agent,
                wave,
                "b+1",
                TriggerState::Pending,
                TriggerState::Answering,
                None,
                3
            )
            .await
            .unwrap()
    );
    // Only one response can start.
    assert!(
        !store
            .advance_trigger(
                &agent,
                wave,
                "b+1",
                TriggerState::Pending,
                TriggerState::Answering,
                None,
                3
            )
            .await
            .unwrap()
    );
    store
        .advance_trigger(
            &agent,
            wave,
            "b+1",
            TriggerState::Answering,
            TriggerState::Answering,
            Some("b+9"),
            4,
        )
        .await
        .unwrap();
    store.note_trigger(&agent, wave, "b+2", 5).await.unwrap();
    let unfinished = store.unfinished_triggers(&agent).await.unwrap();
    assert_eq!(unfinished.len(), 2);
    assert_eq!(unfinished[0].state, TriggerState::Answering);
    assert_eq!(unfinished[0].response_blip_id.as_deref(), Some("b+9"));
    db.close().await;

    // After a restart the states are there; answered ones stay answered.
    let db = test.open();
    let store = db.robot_store();
    store
        .advance_trigger(
            &agent,
            wave,
            "b+1",
            TriggerState::Answering,
            TriggerState::Answered,
            None,
            6,
        )
        .await
        .unwrap();
    assert_eq!(
        store.note_trigger(&agent, wave, "b+1", 7).await.unwrap(),
        TriggerState::Answered
    );
    let trigger = store.trigger(&agent, wave, "b+1").await.unwrap().unwrap();
    assert_eq!(trigger.response_blip_id.as_deref(), Some("b+9"));
    assert_eq!(store.unfinished_triggers(&agent).await.unwrap().len(), 1);
    // Forgetting a trigger lets it be pending again (added a second time).
    store.remove_trigger(&agent, wave, "b+1").await.unwrap();
    assert_eq!(
        store.note_trigger(&agent, wave, "b+1", 8).await.unwrap(),
        TriggerState::Pending
    );
    db.close().await;
}

#[tokio::test]
async fn answered_triggers_keep_their_fingerprint_and_reopen_up_to_a_cap() {
    let test = TestDb::new();
    let db = test.open();
    let store = db.robot_store();
    let agent = participant("helper@example.com");
    let wave = "example.com/w+a/~/example.com/conv+root";
    store.note_trigger(&agent, wave, "b+1", 1).await.unwrap();
    let fresh = store.trigger(&agent, wave, "b+1").await.unwrap().unwrap();
    assert_eq!((fresh.fingerprint, fresh.reanswers), (None, 0));
    let answer = |from, to| store.advance_trigger(&agent, wave, "b+1", from, to, None, 2);
    assert!(
        answer(TriggerState::Pending, TriggerState::Answering)
            .await
            .unwrap()
    );
    store
        .set_trigger_fingerprint(&agent, wave, "b+1", "f1")
        .await
        .unwrap();
    // Not answered yet: cannot be reopened.
    assert!(
        !store
            .reopen_trigger(&agent, wave, "b+1", "f2", 5, 3)
            .await
            .unwrap()
    );
    assert!(
        answer(TriggerState::Answering, TriggerState::Answered)
            .await
            .unwrap()
    );
    db.close().await;

    // The fingerprint survives a restart.
    let db = test.open();
    let store = db.robot_store();
    let found = store.trigger(&agent, wave, "b+1").await.unwrap().unwrap();
    assert_eq!(found.fingerprint.as_deref(), Some("f1"));
    assert_eq!(found.state, TriggerState::Answered);
    // Reopened once: answering again, with the new fingerprint, counted.
    assert!(
        store
            .reopen_trigger(&agent, wave, "b+1", "f2", 2, 4)
            .await
            .unwrap()
    );
    // Only one redo can start.
    assert!(
        !store
            .reopen_trigger(&agent, wave, "b+1", "f3", 2, 4)
            .await
            .unwrap()
    );
    let found = store.trigger(&agent, wave, "b+1").await.unwrap().unwrap();
    assert_eq!(
        (found.state, found.fingerprint.as_deref(), found.reanswers),
        (TriggerState::Answering, Some("f2"), 1)
    );
    let finish = || {
        store.advance_trigger(
            &agent,
            wave,
            "b+1",
            TriggerState::Answering,
            TriggerState::Answered,
            None,
            5,
        )
    };
    finish().await.unwrap();
    assert!(
        store
            .reopen_trigger(&agent, wave, "b+1", "f3", 2, 6)
            .await
            .unwrap()
    );
    finish().await.unwrap();
    // The cap of 2 is reached; 0 would never have allowed one.
    assert!(
        !store
            .reopen_trigger(&agent, wave, "b+1", "f4", 2, 7)
            .await
            .unwrap()
    );
    let found = store.trigger(&agent, wave, "b+1").await.unwrap().unwrap();
    assert_eq!(
        (found.state, found.fingerprint.as_deref(), found.reanswers),
        (TriggerState::Answered, Some("f3"), 2)
    );
    store.note_trigger(&agent, wave, "b+2", 8).await.unwrap();
    store
        .advance_trigger(
            &agent,
            wave,
            "b+2",
            TriggerState::Pending,
            TriggerState::Answered,
            None,
            8,
        )
        .await
        .unwrap();
    assert!(
        !store
            .reopen_trigger(&agent, wave, "b+2", "f", 0, 9)
            .await
            .unwrap()
    );
    db.close().await;
}

#[tokio::test]
async fn usage_adds_up_per_day_and_subject() {
    let test = TestDb::new();
    let db = test.open();
    let store = db.robot_store();
    let usage = |requests, prompt, completion| AgentUsage {
        requests,
        prompt_tokens: prompt,
        completion_tokens: completion,
    };
    store.add_usage(100, "", usage(1, 10, 5)).await.unwrap();
    store.add_usage(100, "", usage(2, 20, 10)).await.unwrap();
    store
        .add_usage(100, "alice@example.com", usage(1, 10, 5))
        .await
        .unwrap();
    store.add_usage(99, "", usage(7, 0, 0)).await.unwrap();
    let mut day = store.usage_of_day(100).await.unwrap();
    day.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        day,
        vec![
            (String::new(), usage(3, 30, 15)),
            ("alice@example.com".to_owned(), usage(1, 10, 5)),
        ]
    );
    store.prune_usage(100).await.unwrap();
    assert!(store.usage_of_day(99).await.unwrap().is_empty());
    assert_eq!(store.usage_of_day(100).await.unwrap().len(), 2);
    db.close().await;
}
