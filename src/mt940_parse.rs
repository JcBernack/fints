use tracing::warn;

use crate::error::{FinTSError, Result};
use crate::types::{Bic, Currency, Iban, Transaction, TransactionStatus};
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use std::str::FromStr;

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

pub(crate) fn mt940_tag_preview(data: &[u8]) -> String {
    if data.is_empty() {
        return "empty".to_owned();
    }

    let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(data);
    let tags = cow
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with(':') {
                trimmed
                    .split_once(':')
                    .and_then(|(_, rest)| rest.split_once(':'))
                    .map(|(tag, _)| format!(":{tag}:"))
            } else {
                None
            }
        })
        .take(12)
        .collect::<Vec<_>>();

    if tags.is_empty() {
        "no MT940 tags found".to_owned()
    } else {
        tags.join(",")
    }
}

pub(crate) fn parse_pending_interim_mt940(data: &[u8]) -> Result<Vec<Transaction>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let (cow, _, had_errors) = encoding_rs::WINDOWS_1252.decode(data);
    if had_errors {
        warn!("pending MT940 encoding errors");
    }

    let lines = cow
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "-" && *line != "--")
        .collect::<Vec<_>>();

    let mut pending_transactions = Vec::new();
    let mut summaries = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if let Some(summary) = parse_summary_line(line) {
            summaries.push(summary);
            index += 1;
            continue;
        }

        let Some(statement_line) = line.strip_prefix(":61:") else {
            index += 1;
            continue;
        };

        let mut purpose = None;
        let mut next_index = index + 1;
        if let Some(next_line) = lines
            .get(next_index)
            .and_then(|line| line.strip_prefix(":86:"))
        {
            purpose = Some(next_line.to_owned());
            next_index += 1;
            while let Some(continuation) = lines.get(next_index) {
                if continuation.starts_with(':') {
                    break;
                }
                if let Some(existing) = purpose.as_mut() {
                    existing.push(' ');
                    existing.push_str(continuation);
                }
                next_index += 1;
            }
        }

        if let Some(transaction) = parse_pending_statement_line(statement_line, purpose) {
            pending_transactions.push(transaction);
        }
        index = next_index;
    }

    validate_pending_summaries(&pending_transactions, &summaries);

    Ok(pending_transactions)
}

#[derive(Debug)]
struct InterimSummary {
    debit: bool,
    count: usize,
    amount: Decimal,
    currency: String,
}

fn parse_summary_line(line: &str) -> Option<InterimSummary> {
    let (debit, value) = line
        .strip_prefix(":90D:")
        .map(|value| (true, value))
        .or_else(|| line.strip_prefix(":90C:").map(|value| (false, value)))?;

    let count_len = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if count_len == 0 || value.len() < count_len + 3 {
        return None;
    }

    let count = value[..count_len].parse().ok()?;
    let currency = value[count_len..count_len + 3].to_owned();
    let amount = Decimal::from_str(&value[count_len + 3..].replace(',', ".")).ok()?;

    Some(InterimSummary {
        debit,
        count,
        amount,
        currency,
    })
}

fn parse_pending_statement_line(
    statement_line: &str,
    purpose: Option<String>,
) -> Option<Transaction> {
    let date = parse_short_date(statement_line.get(..6)?)?;
    let dc_index = statement_line
        .char_indices()
        .skip_while(|(index, _)| *index < 6)
        .find_map(|(index, character)| matches!(character, 'D' | 'C').then_some(index))?;
    let debit = statement_line.as_bytes()[dc_index] == b'D';
    let mut amount_start = dc_index + 1;
    if statement_line[amount_start..]
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
    {
        amount_start += 1;
    }

    let amount_len = statement_line[amount_start..]
        .chars()
        .take_while(|character| {
            character.is_ascii_digit() || *character == ',' || *character == '.'
        })
        .map(char::len_utf8)
        .sum::<usize>();
    if amount_len == 0 {
        return None;
    }

    let amount_text = &statement_line[amount_start..amount_start + amount_len];
    let amount = Decimal::from_str(&amount_text.replace(',', ".")).ok()?;
    let amount = if debit { -amount } else { amount };
    let reference = statement_line
        .get(amount_start + amount_len..)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let raw = serde_json::json!({
        "source": "pending_interim_mt940",
        "statement_line": statement_line,
        "purpose": purpose,
        "reference": reference,
    });

    Some(Transaction {
        date,
        valuta_date: None,
        amount,
        currency: Currency::new("EUR"),
        applicant_name: None,
        applicant_iban: None,
        applicant_bic: None,
        purpose,
        posting_text: None,
        reference,
        raw,
        status: TransactionStatus::Pending,
    })
}

fn parse_short_date(value: &str) -> Option<NaiveDate> {
    if value.len() != 6 || !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }

    let year = value[..2].parse::<i32>().ok()?;
    let current_year = chrono::Utc::now().year();
    let current_century = current_year - current_year.rem_euclid(100);
    let mut full_year = current_century + year;
    if full_year > current_year + 10 {
        full_year -= 100;
    }

    let month = value[2..4].parse().ok()?;
    let day = value[4..6].parse().ok()?;
    NaiveDate::from_ymd_opt(full_year, month, day)
}

fn validate_pending_summaries(transactions: &[Transaction], summaries: &[InterimSummary]) {
    for summary in summaries {
        let matching = transactions.iter().filter(|transaction| {
            if summary.debit {
                transaction.amount.is_sign_negative()
            } else {
                transaction.amount.is_sign_positive()
            }
        });
        let count = matching.clone().count();
        let total = matching.fold(Decimal::ZERO, |total, transaction| {
            if summary.debit {
                total - transaction.amount
            } else {
                total + transaction.amount
            }
        });

        if count != summary.count || total != summary.amount {
            warn!(
                "pending interim MT940 summary mismatch: type={} summary_count={} parsed_count={} summary_amount={} parsed_amount={} currency={}",
                if summary.debit { "debit" } else { "credit" },
                summary.count,
                count,
                summary.amount,
                total,
                summary.currency
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pending_interim_mt940_with_summary_tags() {
        let data = b":20:PENDING\r\n:25:ACCOUNT\r\n:28C:1\r\n:34F:EUR0,\r\n:13D:2607261825+0200\r\n:61:260726D12,34NMSCREF1\r\n:86:Card payment one\r\n:61:260726D5,66NMSCREF2\r\n:86:Card payment two\r\n:61:260726C1,00NMSCREF3\r\n:86:Refund\r\n:90D:2EUR18,00\r\n:90C:1EUR1,00\r\n";

        let transactions = parse_pending_interim_mt940(data).unwrap();

        assert_eq!(transactions.len(), 3);
        assert_eq!(transactions[0].status, TransactionStatus::Pending);
        assert_eq!(transactions[0].amount, Decimal::new(-1234, 2));
        assert_eq!(transactions[0].purpose.as_deref(), Some("Card payment one"));
        assert_eq!(transactions[2].amount, Decimal::new(100, 2));
    }
}
