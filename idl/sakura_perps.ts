/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/sakura_perps.json`.
 */
export type SakuraPerps = {
  "address": "5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y",
  "metadata": {
    "name": "sakuraPerps",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "Permissionless oracle-and-pool perpetual futures on Solana"
  },
  "instructions": [
    {
      "name": "initializeExchange",
      "docs": [
        "Creates the singleton [`Exchange`] configuration account.",
        "",
        "Callable once. The `Exchange` PDA has fixed seeds, so a second call fails",
        "at account creation rather than needing an explicit guard.",
        "",
        "The collateral mint is captured here, along with the token program that",
        "owns it. Every later instruction that moves collateral asserts against",
        "the stored program id — `Interface<TokenInterface>` accepts both the",
        "legacy and Token-2022 programs, so without pinning, a caller could",
        "present the wrong one."
      ],
      "discriminator": [
        224,
        105,
        116,
        166,
        228,
        207,
        96,
        19
      ],
      "accounts": [
        {
          "name": "admin",
          "writable": true,
          "signer": true
        },
        {
          "name": "exchange",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  101,
                  120,
                  99,
                  104,
                  97,
                  110,
                  103,
                  101
                ]
              }
            ]
          }
        },
        {
          "name": "collateralMint",
          "docs": [
            "Collateral mint. `InterfaceAccount` accepts a mint owned by either the",
            "legacy SPL Token program or Token-2022; which one it actually is gets",
            "recorded on the exchange and enforced from then on."
          ]
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "initializeExchangeParams"
            }
          }
        }
      ]
    }
  ],
  "accounts": [
    {
      "name": "exchange",
      "discriminator": [
        30,
        200,
        220,
        149,
        3,
        61,
        104,
        50
      ]
    }
  ],
  "events": [
    {
      "name": "exchangeInitialized",
      "discriminator": [
        200,
        154,
        187,
        160,
        29,
        243,
        230,
        104
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "protocolFeeShareTooHigh",
      "msg": "Protocol fee share exceeds the maximum permitted by the program."
    },
    {
      "code": 6001,
      "name": "collateralMintIsFreezable",
      "msg": "Collateral mint has a freeze authority, which could brick withdrawals and liquidations."
    },
    {
      "code": 6002,
      "name": "mathOverflow",
      "msg": "Arithmetic overflow."
    }
  ],
  "types": [
    {
      "name": "exchange",
      "docs": [
        "Singleton exchange configuration. Seeds: `[b\"exchange\"]`."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "admin",
            "docs": [
              "Current admin. Changed only via the two-step `pending_admin` handshake."
            ],
            "type": "pubkey"
          },
          {
            "name": "pendingAdmin",
            "docs": [
              "Proposed admin, who must call an accept instruction to take over."
            ],
            "type": "pubkey"
          },
          {
            "name": "feeRecipient",
            "docs": [
              "Where the protocol's fee share is sent."
            ],
            "type": "pubkey"
          },
          {
            "name": "collateralMint",
            "docs": [
              "Collateral and settlement mint for every market."
            ],
            "type": "pubkey"
          },
          {
            "name": "collateralTokenProgram",
            "docs": [
              "Token program owning `collateral_mint` — legacy SPL Token or Token-2022.",
              "Pinned at initialization; later instructions must match it exactly."
            ],
            "type": "pubkey"
          },
          {
            "name": "collateralDecimals",
            "docs": [
              "Cached from the mint. Read at runtime, never assumed — a predecessor",
              "program assumed 9 decimals for a 6-decimal mint and was wrong by 1000x."
            ],
            "type": "u8"
          },
          {
            "name": "pausedFlags",
            "docs": [
              "Bitfield of [`PauseFlags`]."
            ],
            "type": "u64"
          },
          {
            "name": "protocolFeeShareBps",
            "docs": [
              "Protocol's share of trading fees in bps; the rest goes to LPs."
            ],
            "type": "u16"
          },
          {
            "name": "numMarkets",
            "docs": [
              "Number of markets created so far."
            ],
            "type": "u32"
          },
          {
            "name": "reserved",
            "docs": [
              "Anchor has no migration story and fields always get added. Reserve now,",
              "because growing an account later means reallocating every instance."
            ],
            "type": {
              "array": [
                "u8",
                128
              ]
            }
          }
        ]
      }
    },
    {
      "name": "exchangeInitialized",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "exchange",
            "type": "pubkey"
          },
          {
            "name": "admin",
            "type": "pubkey"
          },
          {
            "name": "collateralMint",
            "type": "pubkey"
          },
          {
            "name": "collateralTokenProgram",
            "type": "pubkey"
          },
          {
            "name": "collateralDecimals",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "initializeExchangeParams",
      "docs": [
        "Arguments to [`sakura_perps::initialize_exchange`]."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "feeRecipient",
            "docs": [
              "Destination for the protocol's share of trading fees."
            ],
            "type": "pubkey"
          },
          {
            "name": "protocolFeeShareBps",
            "docs": [
              "Protocol's cut of trading fees in bps; the remainder accrues to LPs."
            ],
            "type": "u16"
          }
        ]
      }
    }
  ]
};
