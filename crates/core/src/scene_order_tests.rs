use super::*;
#[test]
fn scene_order_is_one_history_edit_with_identity_selection_noop_and_rollback() {
    let mut session = Session::new();
    let a = session.preview();
    let b = session
        .apply(Command::AddScene {
            name: "Second".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let c = session
        .apply(Command::AddScene {
            name: "Third".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session.select_program(c).unwrap();
    let before = session.snapshot();
    session
        .apply(Command::MoveScene { scene: a, index: 2 })
        .unwrap();
    let moved = session.snapshot();
    assert_eq!(
        moved.scenes().iter().map(Scene::id).collect::<Vec<_>>(),
        [b, c, a]
    );
    assert_eq!((session.preview(), session.program()), (a, c));
    assert_eq!(moved.revision(), before.revision() + 1);
    assert!(
        !session
            .apply(Command::MoveScene { scene: a, index: 2 })
            .unwrap()
            .changed
    );
    assert!(Arc::ptr_eq(&moved, &session.snapshot()));
    assert!(
        session
            .apply(Command::MoveScene { scene: a, index: 3 })
            .is_err()
    );
    assert!(
        session
            .apply(Command::MoveScene {
                scene: Id::new(999),
                index: 0
            })
            .is_err()
    );
    assert!(
        session
            .apply_at(
                Some(before.revision()),
                Command::MoveScene { scene: a, index: 0 }
            )
            .is_err()
    );
    assert!(Arc::ptr_eq(&moved, &session.snapshot()));
    session.apply(Command::Undo).unwrap();
    assert_eq!(session.snapshot().scenes(), before.scenes());
    session.apply(Command::Redo).unwrap();
    assert_eq!(session.snapshot().scenes(), moved.scenes());
    assert_eq!((session.preview(), session.program()), (a, c));
}
