// The open models ai-agent offers: each pinned to one GGUF file at one Hugging Face commit, with its size and SHA-256.

// One model: its short name, where its file lives, what the file must hash to, and what it is good for.
#[derive(Debug, PartialEq)]
pub struct Model {
    pub name: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub about: &'static str,
}

// The model used when none is named and none was chosen with `llm --model`.
pub const DEFAULT: &str = "qwen3.5-4b";

// The catalogue, smallest first; 4-bit quantizations that fit the memory of the Mac they name.
pub const MODELS: &[Model] = &[
    Model {
        name: "qwen3.5-2b",
        repo: "unsloth/Qwen3.5-2B-GGUF",
        revision: "f6d5376be1edb4d416d56da11e5397a961aca8ae",
        file: "Qwen3.5-2B-Q4_K_M.gguf",
        sha256: "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223",
        bytes: 1_280_835_840,
        about: "Qwen 3.5 2B: quick summaries and classification, any Mac",
    },
    Model {
        name: "qwen3.5-4b",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        revision: "e87f176479d0855a907a41277aca2f8ee7a09523",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        bytes: 2_740_937_888,
        about: "Qwen 3.5 4B: the default, good reasoning for its size, 8 GB Macs",
    },
    Model {
        name: "qwen3.5-9b",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        file: "Qwen3.5-9B-Q4_K_M.gguf",
        sha256: "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8",
        bytes: 5_680_522_464,
        about: "Qwen 3.5 9B: general use and code, 16 GB Macs",
    },
    Model {
        name: "gemma-4-12b",
        repo: "unsloth/gemma-4-12b-it-GGUF",
        revision: "fc034cfff751157913579611efad8462ac1be606",
        file: "gemma-4-12b-it-Q4_K_M.gguf",
        sha256: "0a270ec9fe6b34f4a0d33992b6135117b484ebc4766ab76b51d4ae8c457e4c42",
        bytes: 7_121_861_440,
        about: "Gemma 4 12B: writing and many languages, 16 GB Macs",
    },
    Model {
        name: "gpt-oss-20b",
        repo: "ggml-org/gpt-oss-20b-GGUF",
        revision: "41e8a94d15d62070b36cf4553a0250717ecf46b8",
        file: "gpt-oss-20b-MXFP4.gguf",
        sha256: "27cd6c432c7672cb812a92f611cf3ba7bbc35928262bb1e1253ff4ee6ae35901",
        bytes: 12_109_566_624,
        about: "OpenAI gpt-oss 20B: reasoning with native low/medium/high effort, 24 GB Macs",
    },
    Model {
        name: "qwen3.8-27b",
        repo: "unsloth/Qwen3.8-27B-GGUF",
        revision: "4af51b3542f775b0a8e4b310bfb3f17e01c306c9",
        file: "Qwen3.8-27B-UD-Q4_K_M.gguf",
        sha256: "1bce504dfecd84153666928a41d8a66706f0158d3a317bc5196b72819d7dc529",
        bytes: 16_564_259_744,
        about: "Qwen 3.8 27B: the strongest dense model here, code and agents, 32 GB Macs",
    },
    Model {
        name: "gemma-4-26b-a4b",
        repo: "unsloth/gemma-4-26B-A4B-it-GGUF",
        revision: "c099eb48e663fd284577b04978a94ffccb261841",
        file: "gemma-4-26B-A4B-it-UD-Q4_K_M.gguf",
        sha256: "f2c28b3dc4776931ac6f879e11f203dec637ea0f14267a86ec8f6165f63f293f",
        bytes: 16_947_541_728,
        about: "Gemma 4 26B mixture of experts (4B active): fast and broad, 32 GB Macs",
    },
    Model {
        name: "qwen3.6-35b-a3b",
        repo: "unsloth/Qwen3.6-35B-A3B-GGUF",
        revision: "a483e9e6cbd595906af30beda3187c2663a1118c",
        file: "Qwen3.6-35B-A3B-UD-Q4_K_M.gguf",
        sha256: "ac0e2c1189e055faa36eff361580e79c5bd6f8e76bffb4ce547f167d53e31a61",
        bytes: 22_134_528_992,
        about: "Qwen 3.6 35B mixture of experts (3B active): fast with large-model knowledge, 32 GB Macs",
    },
    Model {
        name: "qwen3-coder-next",
        repo: "unsloth/Qwen3-Coder-Next-GGUF",
        revision: "ce09c67b53bc8739eef83fe67b2f5d293c270632",
        file: "Qwen3-Coder-Next-UD-Q4_K_M.gguf",
        sha256: "3a47167ad1bf9e9a79f233f799588d54f85bc65644e72c19251165823e7ae612",
        bytes: 49_301_055_488,
        about: "Qwen3 Coder Next 80B mixture of experts: code, no thinking, 64 GB Macs",
    },
    Model {
        name: "gpt-oss-120b",
        repo: "ggml-org/gpt-oss-120b-GGUF",
        revision: "f59990363fbbb67f01bce03e5e7bf0929e3e320b",
        file: "gpt-oss-120b-MXFP4.gguf",
        sha256: "582bd40f6886200101f4c4ed9f25f3fe80cc14c86e9e2b37746cd8904a0c622d",
        bytes: 63_387_346_208,
        about: "OpenAI gpt-oss 120B: the strongest reasoning here, 96 GB Macs",
    },
];

// Returns the model a name picks: an exact name, else the only name it starts.
pub fn find(name: &str) -> Result<&'static Model, String> {
    if let Some(m) = MODELS.iter().find(|m| m.name == name) {
        return Ok(m);
    }
    let starts: Vec<&Model> = MODELS.iter().filter(|m| m.name.starts_with(name)).collect();
    match starts.as_slice() {
        [m] => Ok(m),
        [] => Err(format!("no model '{name}'; known: {}", names())),
        _ => {
            Err(format!("'{name}' could be {}", starts.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")))
        }
    }
}

// Returns every model name, space-separated.
pub fn names() -> String {
    MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(" ")
}

// Returns a model file's download URL at its pinned commit.
pub fn url(m: &Model) -> String {
    format!("https://huggingface.co/{}/resolve/{}/{}", m.repo, m.revision, m.file)
}

// Formats a byte count in gigabytes with one decimal.
pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks every entry is well formed: unique names, a 40-hex commit, a 64-hex SHA-256, a GGUF file, and size order.
    #[test]
    fn test_catalog_entries() {
        let hex = |s: &str, n: usize| {
            s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        };
        for (i, m) in MODELS.iter().enumerate() {
            assert!(hex(m.revision, 40) && hex(m.sha256, 64) && m.file.ends_with(".gguf"), "{}", m.name);
            assert!(MODELS[..i].iter().all(|o| o.name != m.name && o.bytes <= m.bytes), "{}", m.name);
        }
        assert!(find(DEFAULT).is_ok());
    }

    // Checks names resolve exactly or by a unique prefix, and that the rest fail with the choices.
    #[test]
    fn test_catalog_find() {
        assert_eq!(find("qwen3.5-4b").map(|m| m.name), Ok("qwen3.5-4b"));
        assert_eq!(find("gemma-4-1").map(|m| m.name), Ok("gemma-4-12b"));
        assert!(find("gpt-oss").expect_err("two").contains("gpt-oss-20b, gpt-oss-120b"));
        assert!(find("llama").expect_err("none").contains("known: qwen3.5-2b"));
        assert_eq!(
            url(&MODELS[0]),
            "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/f6d5376be1edb4d416d56da11e5397a961aca8ae/Qwen3.5-2B-Q4_K_M.gguf"
        );
        assert_eq!(gb(2_740_937_888), "2.7 GB");
    }
}
