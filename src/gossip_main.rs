use {
    clap::{crate_description, crate_name, App, Arg},
    cluster_mocks::{
        gossip::{get_crds_table, make_gossip_cluster, Config, CrdsEntry, Node, Packet},
        Error, Router, API_MAINNET_BETA,
    },
    log::info,
    rand::seq::SliceRandom,
    rayon::{prelude::*, ThreadPoolBuilder},
    solana_client::rpc_client::RpcClient,
    solana_sdk::pubkey::Pubkey,
    std::{
        cmp::Reverse,
        collections::HashMap,
        sync::{RwLock, TryLockError},
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
                .default_value("1")
                .help("simulation duration (minutes)"),
        )
        .arg(
            Arg::with_name("gossip_push_fanout")
                .long("gossip-push-fanout")
                .takes_value(true)
                .default_value("5")
                .help("gossip push fanout"),
        )
        .arg(
            Arg::with_name("gossip_push_wide_fanout")
                .long("gossip-push-wide-fanout")
                .takes_value(true)
                .help("gossip push wide fanout"),
        )
        .arg(
            Arg::with_name("gossip_push_capacity")
                .long("gossip-push-capacity")
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
        .arg(
            Arg::with_name("warm_up_rounds")
                .long("warm-up-rounds")
                .takes_value(true)
                .help("number of gossip rounds before collecting stats"),
        )
        .get_matches();

    let json_rpc_url =
        cluster_mocks::get_json_rpc_url(matches.value_of("json_rpc_url").unwrap_or_default());
    info!("json_rpc_url: {}", json_rpc_url);
    let rpc_client = RpcClient::new(json_rpc_url);
    let config = {
        let num_crds = matches.value_of_t_or_exit("num_crds");
        let gossip_push_fanout = matches.value_of_t_or_exit("gossip_push_fanout");
        Config {
            gossip_push_fanout,
            gossip_push_wide_fanout: matches
                .value_of_t("gossip_push_wide_fanout")
                .unwrap_or(gossip_push_fanout),
            gossip_push_capacity: matches.value_of_t_or_exit("gossip_push_capacity"),
            packet_drop_rate: matches.value_of_t_or_exit("packet_drop_rate"),
            num_crds,
            refresh_rate: matches.value_of_t_or_exit("refresh_rate"),
            num_threads: matches
                .value_of_t("num_threads")
                .unwrap_or_else(|_| num_cpus::get()),
            run_duration: Duration::from_secs(
                matches.value_of_t_or_exit::<u64>("run_duration") * 60,
            ),
            warm_up_rounds: matches.value_of_t("warm_up_rounds").unwrap_or(2 * num_crds),
        }
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
    let thread_pool = ThreadPoolBuilder::new()
        .num_threads(config.num_threads)
        .build()
        .unwrap();
    thread_pool
        .broadcast(|_ctx| run_gossip(&config, &nodes, &stakes, &router))
        .into_iter()
        .collect::<Result<Vec<()>, Error>>()
        .unwrap();
    info!("run_gossip done!");
    let mut nodes: Vec<_> = nodes
        .into_iter()
        .map(RwLock::into_inner)
        .collect::<Result<_, _>>()
        .unwrap();
    // Consume packets buffered at each node's receiver channel.
    thread_pool.install(|| {
        nodes.par_iter_mut().for_each(|node| {
            node.consume_packets();
        })
    });
    info!("consume_packets done!");
    // Obtain most recent crds table across all nodes.
    let table = get_crds_table(&nodes);
    info!("num crds entries per node: {}", table.len() / nodes.len());
    // For each node compute how fresh its CRDS table is.
    nodes.sort_unstable_by_key(|node| Reverse(node.stake()));
    let active_stake: u64 = nodes.iter().map(|node| node.stake()).sum();
    println!("node     | stake | rounds |   table | crds");
    println!("-------------------------------------------");
    for node in &nodes {
        let node_table = node.table();
        let num_hits = table
            .iter()
            .filter(|(key, ordinal)| node_table.get(key).map(CrdsEntry::ordinal) == Some(**ordinal))
            .count();
        println!(
            "{} | {:.2}% | {:6} | {:7} | {:2}%",
            &format!("{}", node.pubkey())[..8],
            node.stake() as f64 * 100.0 / active_stake as f64,
            node.num_gossip_rounds(),
            node_table.len(),
            num_hits * 100 / table.len(),
        );
    }
}
