/// The ZNS network. New clients default to testnet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Network {
    #[default]
    Testnet,
    Mainnet,
}

/// Built-in endpoints, registry viewing key and mint treasury address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkConfig {
    pub urls: &'static [&'static str],
    pub viewing_key: &'static str,
    pub treasury_address: &'static str,
}

impl Network {
    pub const fn config(self) -> &'static NetworkConfig {
        match self {
            Self::Testnet => &TESTNET,
            Self::Mainnet => &MAINNET,
        }
    }
}

/// Built-in testnet registry identity and endpoints.
pub const TESTNET: NetworkConfig = NetworkConfig {
    urls: &["https://light.zcash.me/zns-testnet"],
    viewing_key: "uviewtest1gkq78nsemsvs4qekadrv6qlqdmvcv2cct8k3c8cwycedpk90p8rtkx24qe8q73gg2ark8crqmmaavrerf7cxu0sp50an54crmeaalfxl72aetxnr89ksad4lvxzmnkyejhhwn65euzamt9wsukgfemeayhcesreasarmq600lfeju3qenh05e5e5cz6nhhsw3cfdcd3d08d0nxswxmkkxl2fq0hzv6t5dm3ftnz4y76236a49v3us4uq2tlsewf86tjtfwm6kue348me04rm7dw6uawv8myw7t5tyx2t453clhjz5f55lvh7hnvr8g8zt8lf4sy5rzq23ntp7g5ly6kyz2nkwfek06llhjtuz2fft9rg6cuuwg4n7299vajnmq6swg3sf563r0jjzuwuvfpjhmfmydar9dcu3fkuawt6u8qu08vhg5mthrcggrjuhht6f0perh0tqmdfrtgzvhjymay7h9ymvfxgd4r56uz6zrsf5gnvw5jy",
    treasury_address: "utest1tquw9k9qlsphhf30tau2qh758vfvf7xkvjea3rlc5mj4cjscgzfzgjeafjnl7r8tvv0akq3v2ze5q0azf4lnd4vf7kusrfensdsrr7tsgruvuqk0dv0hepnrk0pzgz3fj0mxc0lr29yf65nkwvw74gzep4792rkqa769lxl0sua09s0r",
};

/// Built-in mainnet registry identity and endpoints.
pub const MAINNET: NetworkConfig = NetworkConfig {
    urls: &["https://main.zcashnames.com", "https://light.zcash.me/zns-mainnet"],
    viewing_key: "uview1209ld9y4fr2sel9q87kfjg7xm2sy53e3qf7dvcw3w8jwhhjpldl8z0vk0a6hveg4jk7xcn0rc9mr8yg77djfpc0uxrm5pqzt8q7ul4w0nvs3x5q9vmk9nm2crywtqaealek4mp47ur7ywu7g4g4xcdlwv6gnqzvu5panv5je2ps6rxmx4856unlt5wde8fpz8uhrqchmwhn2j252h3cyx5phad877xezerm6ex8l5kcfjw02ll7m0nspfufamh3ae9ea48395elptspnv6vvc70j2gcjrdze9syt8kpr2kc67flvg8687thdkpha7lpt9r4p9wjty90q2v2yjanqsr2zrecrqs7lfx9cpkmshf7jyh2vvwfqnyzwtpuy7p72hm2mjhperdafvm9u5rwa6s6urcy0uw30f687e3jnmas45qfqtwyz39kf6kxklfrdn7t4hkz56jegmlr8s8hwmt9wa8vz8zlmm76lrmzyj82nzpeqfu572njq",
    treasury_address: "u1f68wxdxsrswjl04kcc397a9ljedg88rl8dgazrk8l9v8jx6t3gqsktvqyvmc3ypgf4pum03q4ftvy5s0fe803nrxm8kw6dgzlvnv83nv",
};
