//! Biometric prompt accounting across add-a-server and cold restart, over the mock credential service.
//! An unlock over an intact vault is one prompt and no writes; a switch with the keyring unlocked is none.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frust::{Component, Set, spawn_local};
use frust_reactive::ReactiveRuntime;

use muxr::MuxrApp;
use muxr::connection::{TrustMode, expect_connection_controller};
use muxr::features::connect::data::repositories::MockCredentialService;
use muxr::features::connect::di::{
    KeyringHandle, expect_credential_service, expect_saved_servers, mock_credentials,
};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::connect::presentation::controller::{
    ConnectController, CredentialOffer, GatePrompt, KeyringUnlock, KeyringUnlockOutcome,
    OfferPrompt,
};
use muxr::features::pro::di::{expect_pro_controller, mock_entitlement};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::features::settings::di::expect_auth_tokens_repository;
use muxr::routes::app_nav;

mod support;
use support::{serial, setup};

const A_HOST: &str = "mock.example";
const A_ENDPOINT: &str = "mock.example:7777";
const B_ENDPOINT: &str = "bravo.example:50051";
const A_TOKEN: &str = "a-vault-token";

fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let runtime = ReactiveRuntime::get().expect("setup() installed the runtime");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn creds() -> Arc<MockCredentialService> {
    mock_credentials().expect("setup() opted into the mock credential store")
}

fn row_a() -> SavedServer {
    SavedServer {
        host: A_HOST.to_string(),
        port: 7777,
        trust_mode: TrustMode::System,
        label: A_HOST.to_string(),
        last_used_ms: epoch_millis(),
        read_only: false,
        has_credential: true,
        token_name: Some("token_a".to_string()),
        fingerprint: None,
    }
}

fn row(endpoint: &str) -> SavedServer {
    expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .into_iter()
        .find(|row| row.endpoint() == endpoint)
        .unwrap_or_else(|| panic!("no saved row for {endpoint}"))
}

/// The app with only A saved (credentialed, in the vault) and Pro unlocked.
fn app_with_a() {
    let _state = MuxrApp.init();
    let saved = expect_saved_servers();
    for seeded in saved.list().expect("the mock store never fails") {
        saved
            .remove(&seeded.endpoint())
            .expect("the mock store never fails");
    }
    saved.save(row_a()).expect("the mock store never fails");
    creds().seed_vault(&[(A_ENDPOINT, A_TOKEN)]);
    mock_entitlement()
        .expect("the scriptable entitlement mock")
        .set_entitlement(Entitlement::Pro);
    expect_pro_controller().entitlement.set(Entitlement::Pro);
}

fn unlocker(keyring: &KeyringHandle) -> Arc<KeyringUnlock> {
    Arc::new(KeyringUnlock::new(
        expect_saved_servers(),
        expect_credential_service(),
        expect_connection_controller(),
        keyring.clone(),
    ))
}

fn unlock(unlocker: &Arc<KeyringUnlock>) {
    let outcome = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&outcome);
    let unlocker = Arc::clone(unlocker);
    spawn_local(async move {
        *slot.lock().unwrap() = Some(unlocker.unlock().await);
    });
    pump_until("the keyring unlock", || outcome.lock().unwrap().is_some());
    assert!(
        matches!(
            outcome.lock().unwrap().take(),
            Some(KeyringUnlockOutcome::Unlocked { .. })
        ),
        "the unlock must succeed"
    );
}

fn connect_controller(keyring: &KeyringHandle, add_mode: bool) -> Arc<ConnectController> {
    let controller = ConnectController::new(
        expect_saved_servers(),
        expect_credential_service(),
        expect_auth_tokens_repository(),
        expect_connection_controller(),
        expect_pro_controller(),
        keyring.clone(),
    );
    Arc::new(if add_mode {
        controller.in_add_mode()
    } else {
        controller
    })
}

fn add_b(controller: &Arc<ConnectController>) {
    let connection = expect_connection_controller();
    connection.set_credential_saver(controller.credential_saver());
    controller.select_trust_mode(TrustMode::NoVerification);
    controller.set_url(B_ENDPOINT.to_string());
    controller.set_token("pasted-b-token".to_string());
    let offered = Rc::new(Cell::new(false));
    let seen = Rc::clone(&offered);
    let offer_prompt: OfferPrompt = Rc::new(move |_offer: CredentialOffer| seen.set(true));
    let gate: GatePrompt = Rc::new(|_block| panic!("a Pro install is never gated"));
    controller.connect(&app_nav(), offer_prompt, gate);
    pump_until("B's offer", || offered.get());
    controller.accept_offer_then_navigate(&app_nav());
    pump_until("B's credential", || row(B_ENDPOINT).has_credential);
}

fn assert_restart_costs_one_prompt_and_b_opens_free() {
    let creds = creds();
    assert!(
        row(B_ENDPOINT).has_credential && row(B_ENDPOINT).token_name.is_some(),
        "B's row claims the credential the offer saved"
    );
    assert!(
        creds
            .vault_endpoints()
            .is_some_and(|e| e.contains(&B_ENDPOINT.to_string())),
        "B's token is in the vault: {:?}",
        creds.vault_endpoints()
    );
    let (prompts_before, writes_before) = (creds.prompts().len(), creds.writes());

    let fresh = KeyringHandle::new();
    expect_connection_controller().disconnect();
    unlock(&unlocker(&fresh));
    let prompts = creds.prompts().len() - prompts_before;
    let writes = creds.writes() - writes_before;
    assert_eq!(
        (prompts, writes),
        (1, 0),
        "unlock = one read, no write; prompts: {:?}",
        creds.prompts()
    );
    assert!(fresh.token_for(B_ENDPOINT).is_some(), "B's token is held");
    assert!(fresh.token_for(A_ENDPOINT).is_some(), "A's token is held");
    assert_eq!(
        creds.prompts().len() - prompts_before,
        1,
        "holding a token for B means opening B asks for nothing more"
    );
}

#[test]
fn adding_b_from_the_add_page_with_the_keyring_unlocked_costs_one_prompt_at_the_next_unlock() {
    let _g = serial();
    let _ambient = setup();
    app_with_a();
    let keyring = KeyringHandle::new();
    unlock(&unlocker(&keyring));
    assert_eq!(creds().prompts().len(), 1, "the first unlock is one prompt");
    assert_eq!(creds().writes(), 0);

    expect_connection_controller().connect(muxr_target_a());
    add_b(&connect_controller(&keyring, true));
    assert_restart_costs_one_prompt_and_b_opens_free();
}

#[test]
fn adding_b_from_the_add_page_with_the_keyring_locked_costs_one_prompt_at_the_next_unlock() {
    let _g = serial();
    let _ambient = setup();
    app_with_a();
    let keyring = KeyringHandle::new();
    expect_connection_controller().connect(muxr_target_a());
    add_b(&connect_controller(&keyring, true));
    assert_restart_costs_one_prompt_and_b_opens_free();
}

#[test]
fn adding_b_from_connects_form_while_disconnected_costs_one_prompt_at_the_next_unlock() {
    let _g = serial();
    let _ambient = setup();
    app_with_a();
    let keyring = KeyringHandle::new();
    unlock(&unlocker(&keyring));
    add_b(&connect_controller(&keyring, false));
    assert_restart_costs_one_prompt_and_b_opens_free();
}

#[test]
fn adding_b_from_connects_form_with_the_keyring_locked_costs_one_prompt_at_the_next_unlock() {
    let _g = serial();
    let _ambient = setup();
    app_with_a();
    let keyring = KeyringHandle::new();
    add_b(&connect_controller(&keyring, false));
    assert_restart_costs_one_prompt_and_b_opens_free();
}

fn muxr_target_a() -> muxr::connection::ConnectTarget {
    muxr::connection::ConnectTarget {
        host: A_HOST.to_string(),
        port: 7777,
        trust_mode: TrustMode::System,
        label: A_HOST.to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(A_TOKEN.to_string()),
        remember: false,
        fingerprint: None,
    }
}

#[test]
fn a_stale_vault_entry_costs_exactly_one_extra_write_at_one_unlock_then_heals() {
    let _g = serial();
    let _ambient = setup();
    app_with_a();
    creds().seed_vault(&[(A_ENDPOINT, A_TOKEN), (B_ENDPOINT, "stale-b-token")]);

    unlock(&unlocker(&KeyringHandle::new()));
    assert_eq!(
        (creds().prompts().len(), creds().writes()),
        (1, 1),
        "one read plus the prune write: two Android prompts"
    );

    unlock(&unlocker(&KeyringHandle::new()));
    assert_eq!(
        (creds().prompts().len(), creds().writes()),
        (2, 1),
        "the vault is clean now: one read, no write"
    );
}
