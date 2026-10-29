use std::str::FromStr;

use rmcp::{
    ErrorData,
    handler::server::wrapper::Parameters,
    model::{GetPromptResult, PromptMessage, Role},
    prompt, prompt_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::domain::Vendor;

use super::{server::X86McpServer, types::vendor_label};

const CITE: &str = "Cite every claim with the vendor, manual (entry_path), and heading path from the returned citations; where the manuals are silent, say so instead of answering from memory. Mention it when a response reports stale: true or a semantic_degraded_reason.";

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ExplainInstructionArgs {
    /// Instruction mnemonic, for example MOVSX, VMRUN, or CVTSI2SD.
    mnemonic: String,
    /// Limit evidence to one vendor: intel or amd. Omit to cover both.
    #[serde(default)]
    vendor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct DescribeRegisterArgs {
    /// Register or MSR name or address, for example CR4, RFLAGS, XCR0, IA32_EFER, or C000_0080h.
    register: String,
    /// Limit evidence to one vendor: intel or amd. Omit to cover both.
    #[serde(default)]
    vendor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CompareVendorsArgs {
    /// Feature or behavior to compare, for example nested paging, SYSCALL MSRs, or APIC virtualization.
    topic: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ResearchArgs {
    /// The x86 architecture question to answer.
    question: String,
    /// Limit evidence to one vendor: intel or amd. Omit to cover both.
    #[serde(default)]
    vendor: Option<String>,
}

#[prompt_router(vis = "pub(crate)")]
impl X86McpServer {
    #[prompt(
        name = "explain_instruction",
        description = "Explain an x86 instruction (operation, encodings, flags, exceptions, CPUID enumeration, Intel/AMD differences) from the Intel SDM and AMD APM, with citations."
    )]
    fn explain_instruction_prompt(
        &self,
        Parameters(args): Parameters<ExplainInstructionArgs>,
    ) -> Result<GetPromptResult, ErrorData> {
        let mnemonic = required("mnemonic", &args.mnemonic)?;
        let vendor = optional_vendor(args.vendor.as_deref())?;
        Ok(user_prompt(
            format!("Explain the x86 instruction {mnemonic}"),
            explain_instruction(mnemonic, vendor),
        ))
    }

    #[prompt(
        name = "describe_register",
        description = "Describe a control register, MSR, or other architectural register (address, bit fields, access rules, CPUID enumeration, Intel/AMD differences) from the Intel SDM and AMD APM, with citations."
    )]
    fn describe_register_prompt(
        &self,
        Parameters(args): Parameters<DescribeRegisterArgs>,
    ) -> Result<GetPromptResult, ErrorData> {
        let register = required("register", &args.register)?;
        let vendor = optional_vendor(args.vendor.as_deref())?;
        Ok(user_prompt(
            format!("Describe the x86 register {register}"),
            describe_register(register, vendor),
        ))
    }

    #[prompt(
        name = "compare_vendors",
        description = "Compare how Intel and AMD document a feature or behavior, with a cited table of differences."
    )]
    fn compare_vendors_prompt(
        &self,
        Parameters(args): Parameters<CompareVendorsArgs>,
    ) -> Result<GetPromptResult, ErrorData> {
        let topic = required("topic", &args.topic)?;
        Ok(user_prompt(
            format!("Compare Intel and AMD on {topic}"),
            compare_vendors(topic),
        ))
    }

    #[prompt(
        name = "research",
        description = "Answer an x86 architecture question from the Intel SDM and AMD APM with cited evidence."
    )]
    fn research_prompt(
        &self,
        Parameters(args): Parameters<ResearchArgs>,
    ) -> Result<GetPromptResult, ErrorData> {
        let question = required("question", &args.question)?;
        let vendor = optional_vendor(args.vendor.as_deref())?;
        Ok(user_prompt(
            "Answer an x86 architecture question from the manuals",
            research(question, vendor),
        ))
    }
}

fn user_prompt(description: impl Into<String>, text: String) -> GetPromptResult {
    GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
        .with_description(description)
}

fn required<'a>(name: &str, value: &'a str) -> Result<&'a str, ErrorData> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ErrorData::invalid_params(
            format!("prompt argument {name} cannot be empty"),
            None,
        ));
    }
    Ok(value)
}

/// Clients send unfilled optional arguments as empty strings, so blank means "both vendors".
fn optional_vendor(value: Option<&str>) -> Result<Option<Vendor>, ErrorData> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => Vendor::from_str(value)
            .map(Some)
            .map_err(|error| ErrorData::invalid_params(format!("prompt argument {error}"), None)),
    }
}

fn scope(vendor: Option<Vendor>) -> String {
    vendor.map_or_else(String::new, |vendor| {
        format!(" (limit evidence to the {} manuals)", vendor_label(vendor))
    })
}

/// Tool arguments, scoped to `vendor` when one is given.
fn with_vendor(mut arguments: Value, vendor: Option<Vendor>) -> Value {
    if let Some(vendor) = vendor {
        arguments["vendors"] = json!([vendor]);
    }
    arguments
}

fn explain_instruction(mnemonic: &str, vendor: Option<Vendor>) -> String {
    let lookup = with_vendor(
        json!({"entity": mnemonic, "kind": "instruction", "limit": 10}),
        vendor,
    );
    let vendors = match vendor {
        Some(_) => String::new(),
        None => format!(
            " If both vendors document it, call x86_compare_vendors with {} and point out every documented difference.",
            json!({"query": mnemonic, "mode": "hybrid", "limit_per_vendor": 5})
        ),
    };
    format!(
        "Explain the x86 instruction {mnemonic} using the Intel SDM and AMD APM through the x86 manual tools{scope}.\n\n\
         1. Call x86_lookup with {lookup}. If entity_state is \"not_found\", say so and list the closest related hits.\n\
         2. Read the instruction reference section of the best exact hit with x86_get_section, following next_cursor until the section is complete; use x86_get_table for opcode and operand-encoding tables.\n\
         3. Cover what the instruction does, its opcode and operand forms, its operation, the flags it affects, the exceptions it raises in each operating mode, and the CPUID feature flag that enumerates it.{vendors}\n\
         4. {CITE}",
        scope = scope(vendor),
    )
}

fn describe_register(register: &str, vendor: Option<Vendor>) -> String {
    let (kind, other) = if is_msr_like(register) {
        ("msr", "register")
    } else {
        ("register", "msr")
    };
    let lookup = with_vendor(
        json!({"entity": register, "kind": kind, "limit": 10}),
        vendor,
    );
    format!(
        "Describe the x86 register {register} using the Intel SDM and AMD APM through the x86 manual tools{scope}.\n\n\
         1. Call x86_lookup with {lookup}. If entity_state is \"not_found\", retry with kind \"{other}\"; if that also fails, say so and list the closest related hits.\n\
         2. Read the defining sections with x86_get_section and fetch bit-layout tables with x86_get_table.\n\
         3. Cover its address or encoding, its width, every bit field (position, name, meaning, reset value, reserved bits), the access rules and the faults raised on invalid access, how CPUID enumerates it or its fields, and the differences between Intel and AMD.\n\
         4. {CITE}",
        scope = scope(vendor),
    )
}

fn compare_vendors(topic: &str) -> String {
    let compare = json!({"query": topic, "mode": "hybrid", "limit_per_vendor": 8});
    format!(
        "Compare how Intel and AMD document {topic}, using the Intel SDM and AMD APM through the x86 manual tools.\n\n\
         1. Call x86_compare_vendors with {compare}. The vendors often name the same concept differently (for example VMX and EPT versus SVM and nested paging); when one side returns weak hits, repeat the call with that vendor's terminology.\n\
         2. Read the strongest sections on each side with x86_get_section.\n\
         3. Answer with what both vendors share, a table of differences (aspect | Intel | AMD) with a citation in each cell, and the vendor-specific names for the same concept. Do not claim the vendors match unless the evidence shows it, and state what either manual leaves undocumented.\n\
         4. {CITE}"
    )
}

fn research(question: &str, vendor: Option<Vendor>) -> String {
    let search = with_vendor(
        json!({"query": "<terms>", "mode": "hybrid", "limit": 10}),
        vendor,
    );
    format!(
        "Answer this x86 architecture question using the Intel SDM and AMD APM through the x86 manual tools{scope}:\n\n\
         {question}\n\n\
         1. Call x86_index_status; if its state is not \"ready\", tell the user what that means for the answer.\n\
         2. Look up exact names first (mnemonics and register, MSR, CPUID leaf, and exception names) with x86_lookup, and search for everything else with x86_search, for example {search}.\n\
         3. Read the most relevant sections with x86_get_section, follow cross-references with x86_get_references when the answer depends on them, and use x86_build_context to collect cited evidence within a token budget.\n\
         4. Answer concisely and keep what the manuals state apart from your own inference. {CITE}",
        scope = scope(vendor),
    )
}

/// `IA32_*`/`MSR_*` names and hex addresses (`C000_0080h`, `0xC0000080`) are MSRs.
fn is_msr_like(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper.starts_with("IA32_") || upper.starts_with("MSR_") {
        return true;
    }
    let digits = upper.strip_prefix("0X").unwrap_or(&upper);
    let digits = digits.strip_suffix('H').unwrap_or(digits);
    let digits = digits.replace('_', "");
    digits.len() >= 3
        && digits
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}
