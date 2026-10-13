use super::*;

fn directory() -> PathBuf {
    let directory = std::env::temp_dir().join(format!("qtr-runs-test-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    directory
}

#[test]
fn intents_survive_restart_and_do_not_leak_requests() {
    let directory = directory();
    let store = RunStore::open(&directory).unwrap();
    assert!(RunStore::open(&directory).is_err());
    store.create("run-a").unwrap();
    let resource = store
        .reserve("run-a", "image", "node.qcow2", "secret-data")
        .unwrap();
    assert_eq!(
        resource.token,
        store
            .reserve("run-a", "image", "node.qcow2", "secret-data")
            .unwrap()
            .token
    );
    assert!(
        store
            .reserve("run-a", "image", "node.qcow2", "changed")
            .is_err()
    );
    store.create("run-b").unwrap();
    assert!(
        store
            .reserve("run-b", "image", "node.qcow2", "secret-data")
            .is_err()
    );
    drop(store);
    let reopened = RunStore::open(&directory).unwrap();
    let run = reopened.get("run-a").unwrap().unwrap();
    assert_eq!(run.resources[0].token, resource.token);
    assert_eq!(run.resources[0].state, "creating");
    let json = serde_json::to_string(&run).unwrap();
    assert!(!json.contains("secret-data"));
    assert!(!json.contains(&resource.token));
    reopened.run_state("run-a", "cleaning").unwrap();
    assert!(
        reopened
            .reserve("run-a", "media", "seed.iso", "{}")
            .is_err()
    );
    reopened.run_state("run-a", "closed").unwrap();
    assert_eq!(reopened.create("run-a").unwrap().state, "closed");
    assert_eq!(reopened.list().unwrap().len(), 2);
    drop(reopened);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pins_survive_publication_and_reject_same_name_replacement() {
    let directory = directory();
    let store = RunStore::open(&directory).unwrap();
    store.create("run-a").unwrap();
    let resource = store.reserve("run-a", "image", "node.qcow2", "{}").unwrap();
    let staging = directory.join("staging");
    let destination = directory.join(&resource.id);
    fs::write(&staging, "owned").unwrap();
    pin_file(&directory, &resource, &staging).unwrap();
    assert!(!check_file(&directory, &resource).unwrap());
    fs::hard_link(&staging, &destination).unwrap();
    fs::remove_file(&staging).unwrap();
    drop(store);
    let store = RunStore::open(&directory).unwrap();
    let recovered = store.get("run-a").unwrap().unwrap().resources.remove(0);
    assert!(check_file(&directory, &recovered).unwrap());
    fs::remove_file(&destination).unwrap();
    fs::write(&destination, "foreign").unwrap();
    assert!(check_file(&directory, &recovered).is_err());
    assert_eq!(fs::read_to_string(&destination).unwrap(), "foreign");
    fs::remove_file(&destination).unwrap();
    unpin_file(&directory, &recovered).unwrap();
    unpin_file(&directory, &recovered).unwrap();
    fs::write(&destination, "foreign").unwrap();
    assert!(check_file(&directory, &recovered).is_err());
    fs::remove_file(&destination).unwrap();
    std::os::unix::fs::symlink("missing-target", &destination).unwrap();
    assert!(check_file(&directory, &recovered).is_err());
    drop(store);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn rejects_unsafe_ids_and_public_state_directories() {
    for id in ["", "../escape", "/absolute", "a/b", "a b", "a\n", ".hidden"] {
        assert!(validate_run_id(id).is_err());
    }
    assert!(validate_run_id(&"a".repeat(64)).is_err());
    let directory = directory();
    fs::create_dir(directory.join("runs")).unwrap();
    fs::set_permissions(directory.join("runs"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(RunStore::open(&directory).is_err());
    fs::remove_dir_all(directory).unwrap();
}
