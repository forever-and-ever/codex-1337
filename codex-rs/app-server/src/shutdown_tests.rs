use super::*;
use pretty_assertions::assert_eq;

#[test]
fn shutdown_waits_for_admitted_requests_but_force_remains_available() {
    let admission = TurnAdmission::default();
    let mut active = admission.subscribe_active();
    let (_running_tx, mut running) = watch::channel(/*init*/ 0);
    let in_flight = admission.admit().expect("request admitted");
    let mut shutdown = ShutdownState::default();
    #[cfg(unix)]
    shutdown.on_signal(
        ShutdownSignal::WhenIdle,
        /*connection_count*/ 1,
        /*running_turn_count*/ 0,
        &admission,
    );
    shutdown.on_signal(
        ShutdownSignal::Forceable,
        /*connection_count*/ 1,
        /*running_turn_count*/ 0,
        &admission,
    );
    assert_eq!(
        shutdown.update(
            &mut running,
            &mut active,
            /*connection_count*/ 1,
            &admission
        ),
        ShutdownAction::Noop
    );
    assert!(!shutdown.forced());
    assert!(admission.admit().is_err());
    shutdown.on_signal(
        ShutdownSignal::Forceable,
        /*connection_count*/ 1,
        /*running_turn_count*/ 0,
        &admission,
    );
    assert!(shutdown.forced());
    assert_eq!(
        shutdown.update(
            &mut running,
            &mut active,
            /*connection_count*/ 1,
            &admission
        ),
        ShutdownAction::Finish
    );
    drop(in_flight);
}

#[cfg(unix)]
#[test]
fn pending_upgrade_keeps_admission_open_until_requests_and_turns_are_idle() {
    let admission = TurnAdmission::default();
    let mut active = admission.subscribe_active();
    let (running_tx, mut running) = watch::channel(/*init*/ 0);
    let request = admission.admit().expect("starting a turn");
    let mut shutdown = ShutdownState::default();
    for _ in 0..2 {
        shutdown.on_signal(
            ShutdownSignal::WhenIdle,
            /*connection_count*/ 1,
            /*running_turn_count*/ 0,
            &admission,
        );
        assert_eq!(
            shutdown.update(
                &mut running,
                &mut active,
                /*connection_count*/ 1,
                &admission
            ),
            ShutdownAction::Noop
        );
    }
    // Admission and running-turn publication overlap, not a transient idle gap.
    running_tx.send_replace(/*value*/ 1);
    drop(request);
    assert_eq!(
        shutdown.update(
            &mut running,
            &mut active,
            /*connection_count*/ 1,
            &admission
        ),
        ShutdownAction::Noop
    );
    let resumed = admission.admit().expect("resumes still admitted");
    running_tx.send_replace(/*value*/ 0);
    assert_eq!(
        shutdown.update(
            &mut running,
            &mut active,
            /*connection_count*/ 1,
            &admission
        ),
        ShutdownAction::Noop
    );
    drop(resumed);
    assert_eq!(
        shutdown.update(
            &mut running,
            &mut active,
            /*connection_count*/ 1,
            &admission
        ),
        ShutdownAction::Finish
    );
    assert!(admission.admit().is_err());
}
