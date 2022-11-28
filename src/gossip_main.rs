use {
    clap::{crate_description, crate_name, App, Arg},
    cluster_mocks::{
        gossip::{get_crds_table, make_gossip_cluster, Config, Node, Packet},
        Error, Router, API_MAINNET_BETA, API_TESTNET,
    },
    log::info,
    rand::seq::SliceRandom,
    solana_client::rpc_client::RpcClient,
    solana_sdk::pubkey::Pubkey,
    std::{
        cmp::Reverse,
        collections::HashMap,
        iter::repeat_with,
        sync::{Arc, RwLock, TryLockError},
        thread::spawn,
        time::{Duration, Instant},
    },
};

fn run_gossip(
    config: &Config,
    nodes: &[RwLock<Node>],
    stakes: &HashMap<Pubkey, /*stake:*/ u64>,
    router: &Router<Packet>,
) -> Result<(), Error> {
    let mut rng = rand::thread_rng();
    let now = Instant::now();
    while now.elapsed() < config.run_duration {
        let node = nodes.choose(&mut rng).unwrap();
        let mut node = match node.try_write() {
            Ok(node) => node,
            Err(TryLockError::Poisoned(_)) => return Err(Error::TryLockErrorPoisoned),
            Err(TryLockError::WouldBlock) => continue,
        };
        node.run_gossip(&mut rng, config, stakes, router)?;
    }
    Ok(())
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "INFO");
    }
    solana_logger::setup();

    let matches = App::new(crate_name!())
        .about(crate_description!())
        .arg(
            Arg::with_name("json_rpc_url")
                .long("url")
                .value_name("URL_OR_MONIKER")
                .takes_value(true)
                .default_value(API_MAINNET_BETA)
                .help("solana's json rpc url"),
        )
        .arg(
            Arg::with_name("num_threads")
                .long("num-threads")
                .takes_value(true)
                .help("number of worker threads"),
        )
        .arg(
            Arg::with_name("run_duration")
                .long("run-duration")
                .takes_value(true)
                .default_value("20")
                .help("simulation duration (seconds)"),
        )
        .arg(
            Arg::with_name("gossip_push_fanout")
                .long("gossip-push-fanout")
                .takes_value(true)
                .default_value("5")
                .help("gossip push fanout"),
        )
        .arg(
            Arg::with_name("packet_drop_rate")
                .long("packet-drop-rate")
                .takes_value(true)
                .default_value("0.0")
                .help("packet drop probability"),
        )
        .arg(
            Arg::with_name("num_crds")
                .long("num-crds")
                .takes_value(true)
                .default_value("256")
                .help("num of CRDS values per node"),
        )
        .arg(
            Arg::with_name("refresh_rate")
                .long("refresh-rate")
                .takes_value(true)
                .default_value("4")
                .help("number of new CRDS value to generate per gossip round"),
        )
        .get_matches();

    let json_rpc_url = match matches.value_of("json_rpc_url").unwrap_or_default() {
        "m" | "mainnet-beta" => API_MAINNET_BETA,
        "t" | "testnet" => API_TESTNET,
        url => url,
    };
    info!("json_rpc_url: {}", json_rpc_url);
    let rpc_client = RpcClient::new(json_rpc_url);
    let config = Config {
        gossip_push_fanout: matches.value_of_t_or_exit("gossip_push_fanout"),
        packet_drop_rate: matches.value_of_t_or_exit("packet_drop_rate"),
        num_crds: matches.value_of_t_or_exit("num_crds"),
        refresh_rate: matches.value_of_t_or_exit("refresh_rate"),
        num_threads: matches.value_of_t("num_threads").unwrap_or(num_cpus::get()),
        run_duration: Duration::from_secs(matches.value_of_t_or_exit("run_duration")),
    };
    info!("config: {:#?}", config);
    assert!(config.num_threads > 0);
    let nodes = make_gossip_cluster(&rpc_client).unwrap();
    let (nodes, senders): (Vec<_>, Vec<_>) = nodes
        .into_iter()
        .map(|(node, sender)| {
            let pubkey = node.pubkey();
            (node, (pubkey, sender))
        })
        .unzip();
    let router = Router::new(config.packet_drop_rate, senders).unwrap();
    // TODO: remove unstaked here?!
    let stakes: HashMap<Pubkey, /*stake:*/ u64> = nodes
        .iter()
        .map(|node| (node.pubkey(), node.stake()))
        .collect();
    let nodes: Vec<_> = nodes.into_iter().map(RwLock::new).collect();
    let nodes = Arc::new(nodes);
    let router = Arc::new(router);
    let stakes = Arc::new(stakes);
    let handles: Vec<_> = repeat_with(|| {
        let nodes = nodes.clone();
        let router = router.clone();
        let stakes = stakes.clone();
        spawn(move || run_gossip(&config, &nodes, &stakes, &router))
    })
    .take(config.num_threads)
    .collect();
    info!("started {} threads", handles.len());
    handles
        .into_iter()
        .try_for_each(|handle| handle.join().unwrap())
        .unwrap();
    info!("joined threads");
    // Obtain most recent crds table across all nodes.
    // TODO: need threadpool here! or do it in the run threads!
    let mut nodes: Vec<_> = Arc::try_unwrap(nodes)
        .unwrap()
        .into_iter()
        .map(|node| {
            let mut node = node.into_inner().unwrap();
            node.consume_packets();
            node
        })
        .collect();
    let table = get_crds_table(&nodes);
    info!("num crds entries per node: {}", table.len() / nodes.len());
    // For each node compute how fresh its CRDS table is.
    nodes.sort_unstable_by_key(|node| Reverse(node.stake()));
    for node in &nodes {
        let node_table = node.table();
        let num_hits = table
            .iter()
            .filter(|(key, ordinal)| node_table.get(key) == Some(ordinal))
            .count();
        // TODO: also add stake%.
        info!(
            "{}: crds: {}, {}%, buffered: {}",
            &format!("{}", node.pubkey())[..8],
            node_table.len(),
            num_hits * 100 / table.len(),
            node.num_buffered(),
        );
    }
}
