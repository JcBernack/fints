use tracing::warn;

use crate::error::{FinTSError, Result};
use crate::types::{Bic, Currency, Iban, Transaction, TransactionStatus};

pub(crate) fn parse_mt940(data: &[u8], status: TransactionStatus) -> Result<Vec<Transaction>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let (cow, _, had_errors) = encoding_rs::WINDOWS_1252.decode(data);
    if had_errors {
        warn!("MT940 encoding errors");
    }
    let mt940_text = cow.into_owned();

    let cleaned: String = mt940_text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && trimmed != "-" && trimmed != "--"
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";

    let sanitized = mt940::sanitizers::to_swift_charset(&cleaned);
    let messages = mt940::parse_mt940(&sanitized)
        .map_err(|error| FinTSError::Mt940(format!("MT940 parse error: {error}")))?;

    let mut transactions = Vec::new();
    for message in messages {
        for line in message.statement_lines {
            let is_debit = matches!(
                line.ext_debit_credit_indicator,
                mt940::ExtDebitOrCredit::Debit
            );
            let amount = if is_debit { -line.amount } else { line.amount };

            let (applicant_name, applicant_iban, applicant_bic, purpose, posting_text) =
                match &line.information_to_account_owner {
                    Some(mt940::InformationToAccountOwner::Structured {
                        applicant_name,
                        applicant_iban,
                        applicant_bin,
                        purpose,
                        posting_text,
                        ..
                    }) => (
                        applicant_name.clone(),
                        applicant_iban.clone(),
                        applicant_bin.clone(),
                        purpose.clone(),
                        posting_text.clone(),
                    ),
                    Some(mt940::InformationToAccountOwner::Plain(text)) => {
                        (None, None, None, Some(text.clone()), None)
                    }
                    None => (None, None, None, None, None),
                };

            let raw = serde_json::json!({
                "date": line.value_date.to_string(),
                "entry_date": line.entry_date.map(|date| date.to_string()),
                "amount": amount.to_string(),
                "currency": message.opening_balance.iso_currency_code,
                "customer_ref": line.customer_ref,
                "bank_ref": line.bank_ref,
                "applicant_name": applicant_name,
                "applicant_iban": applicant_iban,
                "applicant_bic": applicant_bic,
                "purpose": purpose,
                "posting_text": posting_text,
            });

            transactions.push(Transaction {
                date: line.value_date,
                valuta_date: line.entry_date,
                amount,
                currency: Currency::new(&message.opening_balance.iso_currency_code),
                applicant_name,
                applicant_iban: applicant_iban.map(Iban::new),
                applicant_bic: applicant_bic.map(Bic::new),
                purpose,
                posting_text,
                reference: Some(line.customer_ref.clone()),
                raw,
                status: status.clone(),
            });
        }
    }

    Ok(transactions)
}
