use {
    clap::{crate_description, crate_name, App, Arg},
    cluster_mocks::{gossip::make_gossip_cluster, API_MAINNET_BETA},
    log::info,
    rand::Rng,
    solana_client::rpc_client::RpcClient,
    solana_gossip::{crds_gossip, weighted_shuffle::WeightedShuffle},
    solana_sdk::pubkey::Pubkey,
    std::{
        cmp::Reverse,
        collections::HashMap,
        time::{Duration, Instant},
    },
};

#[derive(Debug)]
struct Config {
    gossip_push_fanout: usize,
    num_rounds: usize,
    round_delay: Duration,
}

fn run_sample_peers<R: Rng>(rng: &mut R, config: &Config, stakes: &HashMap<Pubkey, u64>) {
    const MAX_WEIGHT: f32 = u16::MAX as f32 - 1.0;
    let mut now = Instant::now();
    let mut hits = HashMap::<Pubkey, usize>::with_capacity(stakes.len());
    let mut touch = HashMap::<Pubkey, Instant>::with_capacity(stakes.len());
    for _ in 0..config.num_rounds {
        let (nodes, weights): (Vec<_>, Vec<_>) = stakes
            .keys()
            .map(|&pubkey| {
                let stake = crds_gossip::get_stake(&pubkey, stakes);
                let since = touch
                    .get(&pubkey)
                    .map(|&t| (now - t).as_millis())
                    .unwrap_or(u128::MAX)
                    .min(3600 * 1000);
                let since = (since / 1024) as u32;
                let weight = crds_gossip::get_weight(MAX_WEIGHT, since, stake);
                (pubkey, (weight * 100.0) as u64)
            })
            .unzip();
        let shuffle = WeightedShuffle::new("run-sample-peers", &weights).shuffle(rng);
        for k in shuffle.take(config.gossip_push_fanout) {
            let node = nodes[k];
            *hits.entry(node).or_default() += 1;
            touch.insert(node, now);
        }
        now += config.round_delay;
    }
    let active_stake: u64 = stakes.values().sum();
    let mut hits: Vec<_> = stakes
        .iter()
        .map(|(&pubkey, &stake)| {
            let hits = hits.get(&pubkey).copied().unwrap_or_default();
            (pubkey, stake, hits)
        })
        .collect();
    hits.sort_unstable_by_key(|(_pubkey, stake, _hits)| Reverse(*stake));
    println!("node     | stake  | stake | hits");
    for (pubkey, stake, hits) in hits {
        println!(
            "{} | {:.3}% | {:5.2} | {:5}",
            &format!("{}", pubkey)[..8],
            stake as f64 * 100.0 / active_stake as f64,
            crds_gossip::get_stake(&pubkey, stakes),
            hits
        );
    }
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

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
            Arg::with_name("gossip_push_fanout")
                .long("gossip-push-fanout")
                .takes_value(true)
                .default_value("5")
                .help("gossip push fanout"),
        )
        .arg(
            Arg::with_name("num_rounds")
                .long("num-rounds")
                .takes_value(true)
                .default_value("10000")
                .help("number of rounds to simulate"),
        )
        .arg(
            Arg::with_name("round_delay")
                .long("round-delay")
                .takes_value(true)
                .default_value("200")
                .help("delay between rounds [ms]"),
        )
        .get_matches();
    let config = Config {
        gossip_push_fanout: matches.value_of_t_or_exit("gossip_push_fanout"),
        num_rounds: matches.value_of_t_or_exit("num_rounds"),
        round_delay: Duration::from_millis(matches.value_of_t_or_exit("round_delay")),
    };
    info!("config: {:#?}", config);
    let json_rpc_url =
        cluster_mocks::get_json_rpc_url(matches.value_of("json_rpc_url").unwrap_or_default());
    info!("json_rpc_url: {}", json_rpc_url);
    let rpc_client = RpcClient::new(json_rpc_url);
    let stakes: HashMap<Pubkey, /*stake:*/ u64> = make_gossip_cluster(&rpc_client)
        .unwrap()
        .into_iter()
        .map(|(node, _sender)| (node.pubkey(), node.stake()))
        .collect();
    let mut rng = rand::thread_rng();
    run_sample_peers(&mut rng, &config, &stakes);
}
