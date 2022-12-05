use {
    clap::{crate_description, crate_name, App, Arg},
    log::info,
    rand::Rng,
    std::collections::VecDeque,
};

#[derive(Debug)]
struct Config {
    gossip_push_fanout: usize,
    gossip_push_wide_fanout: usize,
    cluster_size: usize,
    num_rounds: usize,
}

fn run_fanout<R: Rng>(rng: &mut R, config: &Config) {
    let mut queue = VecDeque::with_capacity(config.cluster_size);
    let mut seen = vec![false; config.cluster_size];
    let mut nodes: Vec<_> = (0..config.cluster_size).collect();
    let mut num_packets: usize = 0;
    let mut num_outdated: usize = 0;
    let mut num_seen: usize = 0;
    for _ in 0..config.num_rounds {
        queue.clear();
        seen.fill(false);
        seen[0] = true;
        queue.push_back(0);
        while let Some(node) = queue.pop_front() {
            let gossip_push_fanout = if node == 0 {
                config.gossip_push_wide_fanout
            } else {
                config.gossip_push_fanout
            };
            for i in 0..gossip_push_fanout {
                let j = rng.gen_range(i, config.cluster_size);
                nodes.swap(i, j);
            }
            for &other in &nodes[..gossip_push_fanout] {
                if other == node {
                    continue;
                }
                num_packets += 1;
                if seen[other] {
                    num_outdated += 1;
                } else {
                    seen[other] = true;
                    queue.push_back(other);
                }
            }
        }
        num_seen += seen.iter().filter(|k| **k).count();
    }
    let num_rounds = config.num_rounds as f64;
    let packets_node = num_packets as f64 / config.cluster_size as f64 / num_rounds;
    println!("packets/node: {:.0}", packets_node);
    let outdated = num_outdated as f64 * 100.0 / num_packets as f64;
    println!("outdated:     {:.2}%", outdated);
    let waste = num_outdated as f64 / (num_packets - num_outdated) as f64;
    println!("waste:        {:.1}", waste);
    let propagation = num_seen as f64 * 100.0 / config.cluster_size as f64 / num_rounds;
    println!("propagation:  {:.2}%", propagation);
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

    let matches = App::new(crate_name!())
        .about(crate_description!())
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
            Arg::with_name("cluster_size")
                .long("cluset-size")
                .takes_value(true)
                .default_value("3000")
                .help("number of nodes in the cluster"),
        )
        .arg(
            Arg::with_name("num_rounds")
                .long("num-rounds")
                .takes_value(true)
                .default_value("10000")
                .help("number of rounds to simulate"),
        )
        .get_matches();
    let config = {
        let gossip_push_fanout = matches.value_of_t_or_exit("gossip_push_fanout");
        Config {
            gossip_push_fanout,
            gossip_push_wide_fanout: matches
                .value_of_t("gossip_push_wide_fanout")
                .unwrap_or(gossip_push_fanout),
            cluster_size: matches.value_of_t_or_exit("cluster_size"),
            num_rounds: matches.value_of_t_or_exit("num_rounds"),
        }
    };
    info!("config: {:#?}", config);
    let mut rng = rand::thread_rng();
    run_fanout(&mut rng, &config);
}
