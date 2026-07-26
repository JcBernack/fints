use chrono::{DateTime, NaiveDate, Utc};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::error::{FinTSError, Result};

#[derive(Debug, Clone, Default)]
pub struct CamtAccount {
    pub iban: Option<String>,
    pub other_id: Option<String>,
    pub currency: Option<String>,
    pub bic: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CamtBalance {
    pub code: String,
    pub amount: Decimal,
    pub currency: String,
    pub credit_debit: String,
    pub date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default)]
pub struct CamtCode {
    pub domain: Option<String>,
    pub family: Option<String>,
    pub subfamily: Option<String>,
    pub proprietary: Option<String>,
    pub issuer: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CamtParty {
    pub name: Option<String>,
    pub iban: Option<String>,
    pub bic: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CamtTransaction {
    pub amount: Decimal,
    pub currency: String,
    pub credit_debit: String,
    pub end_to_end_id: Option<String>,
    pub transaction_at: Option<DateTime<Utc>>,
    pub debtor: CamtParty,
    pub creditor: CamtParty,
    pub purpose_code: Option<String>,
    pub remittance: Option<String>,
    pub code: CamtCode,
    pub original_amount: Option<Decimal>,
    pub original_currency: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CamtEntry {
    pub amount: Decimal,
    pub currency: String,
    pub credit_debit: String,
    pub status: String,
    pub booking_date: Option<NaiveDate>,
    pub value_date: Option<NaiveDate>,
    pub acct_svcr_ref: Option<String>,
    pub code: CamtCode,
    pub additional_info: Option<String>,
    pub transactions: Vec<CamtTransaction>,
}

#[derive(Debug, Clone, Default)]
pub struct CamtReport {
    pub account: CamtAccount,
    pub balances: Vec<CamtBalance>,
    pub entries: Vec<CamtEntry>,
    pub page_number: Option<u32>,
    pub last_page: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
enum Context {
    None,
    Balance,
    Entry,
    Transaction,
}

pub fn parse_report(xml: &[u8], pending: bool) -> Result<CamtReport> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut path = Vec::<String>::new();
    let mut report = CamtReport::default();
    let mut balance: Option<CamtBalance> = None;
    let mut entry: Option<CamtEntry> = None;
    let mut transaction: Option<CamtTransaction> = None;
    let mut amount_currency: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                let name = local_name(start.name().as_ref());
                path.push(name.clone());
                match name.as_str() {
                    "Bal" => {
                        balance = Some(CamtBalance {
                            code: String::new(),
                            amount: Decimal::ZERO,
                            currency: String::new(),
                            credit_debit: String::new(),
                            date: None,
                        });
                    }
                    "Ntry" => {
                        entry = Some(CamtEntry {
                            amount: Decimal::ZERO,
                            currency: String::new(),
                            credit_debit: String::new(),
                            status: if pending { "PDNG" } else { "BOOK" }.to_owned(),
                            booking_date: None,
                            value_date: None,
                            acct_svcr_ref: None,
                            code: CamtCode::default(),
                            additional_info: None,
                            transactions: Vec::new(),
                        });
                    }
                    "TxDtls" => {
                        transaction = Some(CamtTransaction {
                            amount: Decimal::ZERO,
                            currency: String::new(),
                            credit_debit: String::new(),
                            end_to_end_id: None,
                            transaction_at: None,
                            debtor: CamtParty::default(),
                            creditor: CamtParty::default(),
                            purpose_code: None,
                            remittance: None,
                            code: CamtCode::default(),
                            original_amount: None,
                            original_currency: None,
                        });
                    }
                    "Amt" => amount_currency = attribute_value(&start, "Ccy"),
                    _ => {}
                }
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Text(text)) => {
                let value = text
                    .unescape()
                    .map_err(|error| parse_error(error.to_string()))?
                    .into_owned();
                assign_text(
                    &path,
                    &value,
                    amount_currency.as_deref(),
                    &mut report,
                    &mut balance,
                    &mut entry,
                    &mut transaction,
                )?;
                amount_currency = None;
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref()).into_owned();
                assign_text(
                    &path,
                    &value,
                    amount_currency.as_deref(),
                    &mut report,
                    &mut balance,
                    &mut entry,
                    &mut transaction,
                )?;
                amount_currency = None;
            }
            Ok(Event::End(end)) => {
                let name = local_name(end.name().as_ref());
                match name.as_str() {
                    "TxDtls" => {
                        if let (Some(entry), Some(transaction)) =
                            (entry.as_mut(), transaction.take())
                        {
                            entry.transactions.push(transaction);
                        }
                    }
                    "Ntry" => {
                        if let Some(entry) = entry.take() {
                            report.entries.push(entry);
                        }
                    }
                    "Bal" => {
                        if let Some(balance) = balance.take() {
                            report.balances.push(balance);
                        }
                    }
                    _ => {}
                }
                path.pop();
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(parse_error(error.to_string())),
            _ => {}
        }
        buffer.clear();
    }

    if report.account.currency.is_none() {
        report.account.currency = report
            .balances
            .first()
            .map(|balance| balance.currency.clone())
            .or_else(|| report.entries.first().map(|entry| entry.currency.clone()));
    }
    normalize_report(&mut report);
    Ok(report)
}

fn assign_text(
    path: &[String],
    value: &str,
    currency: Option<&str>,
    report: &mut CamtReport,
    balance: &mut Option<CamtBalance>,
    entry: &mut Option<CamtEntry>,
    transaction: &mut Option<CamtTransaction>,
) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }

    if path_ends(path, &["Acct", "Ccy"]) {
        report.account.currency = Some(value.to_owned());
    } else if path_ends(path, &["Acct", "Id", "IBAN"]) {
        report.account.iban = Some(value.to_owned());
    } else if path_ends(path, &["Acct", "Id", "Othr", "Id"]) {
        report.account.other_id = Some(value.to_owned());
    } else if path_ends(path, &["Svcr", "FinInstnId", "BICFI"]) {
        report.account.bic = Some(value.to_owned());
    } else if path_ends(path, &["MsgPgntn", "PgNb"]) {
        report.page_number = Some(
            value
                .parse()
                .map_err(|error| parse_error(format!("invalid CAMT page number: {error}")))?,
        );
    } else if path_ends(path, &["MsgPgntn", "LastPgInd"]) {
        report.last_page = Some(
            value
                .parse()
                .map_err(|error| parse_error(format!("invalid CAMT last-page flag: {error}")))?,
        );
    }

    match context(path) {
        Context::Balance => assign_balance(path, value, currency, balance)?,
        Context::Entry => assign_entry(path, value, currency, entry)?,
        Context::Transaction => assign_transaction(path, value, currency, transaction)?,
        Context::None => {}
    }
    Ok(())
}

fn assign_balance(
    path: &[String],
    value: &str,
    currency: Option<&str>,
    balance: &mut Option<CamtBalance>,
) -> Result<()> {
    let Some(balance) = balance.as_mut() else {
        return Ok(());
    };
    if path_ends(path, &["CdOrPrtry", "Cd"]) {
        balance.code = value.to_owned();
    } else if path_ends(path, &["Amt"]) {
        balance.amount = decimal(value)?;
        balance.currency = currency.unwrap_or_default().to_owned();
    } else if path_ends(path, &["CdtDbtInd"]) {
        balance.credit_debit = value.to_owned();
    } else if path_ends(path, &["Dt", "Dt"]) {
        balance.date = Some(date(value)?);
    }
    Ok(())
}

fn assign_entry(
    path: &[String],
    value: &str,
    currency: Option<&str>,
    entry: &mut Option<CamtEntry>,
) -> Result<()> {
    let Some(entry) = entry.as_mut() else {
        return Ok(());
    };
    if path_ends(path, &["Amt"]) {
        entry.amount = decimal(value)?;
        entry.currency = currency.unwrap_or_default().to_owned();
    } else if path_ends(path, &["CdtDbtInd"]) {
        entry.credit_debit = value.to_owned();
    } else if path_ends(path, &["Sts", "Cd"]) {
        entry.status = value.to_owned();
    } else if path_ends(path, &["BookgDt", "Dt"]) {
        entry.booking_date = Some(date(value)?);
    } else if path_ends(path, &["ValDt", "Dt"]) {
        entry.value_date = Some(date(value)?);
    } else if path_ends(path, &["AcctSvcrRef"]) {
        entry.acct_svcr_ref = Some(value.to_owned());
    } else if path_ends(path, &["AddtlNtryInf"]) {
        entry.additional_info = Some(value.to_owned());
    } else {
        assign_code(path, value, &mut entry.code);
    }
    Ok(())
}

fn assign_transaction(
    path: &[String],
    value: &str,
    currency: Option<&str>,
    transaction: &mut Option<CamtTransaction>,
) -> Result<()> {
    let Some(transaction) = transaction.as_mut() else {
        return Ok(());
    };
    if path_ends(path, &["InstdAmt", "Amt"]) {
        transaction.original_amount = Some(decimal(value)?);
        transaction.original_currency = currency.map(ToOwned::to_owned);
    } else if path_ends(path, &["Amt"]) {
        transaction.amount = decimal(value)?;
        transaction.currency = currency.unwrap_or_default().to_owned();
    } else if path_ends(path, &["CdtDbtInd"]) {
        transaction.credit_debit = value.to_owned();
    } else if path_ends(path, &["EndToEndId"]) {
        transaction.end_to_end_id = Some(value.to_owned());
    } else if path_ends(path, &["TxDtTm"]) {
        transaction.transaction_at = Some(
            DateTime::parse_from_rfc3339(value)
                .map_err(|error| parse_error(error.to_string()))?
                .with_timezone(&Utc),
        );
    } else if path_ends(path, &["Dbtr", "Pty", "Nm"]) {
        transaction.debtor.name = Some(value.to_owned());
    } else if path_ends(path, &["DbtrAcct", "Id", "IBAN"]) {
        transaction.debtor.iban = Some(value.to_owned());
    } else if path_ends(path, &["DbtrAgt", "FinInstnId", "BICFI"]) {
        transaction.debtor.bic = Some(value.to_owned());
    } else if path_ends(path, &["Cdtr", "Pty", "Nm"]) {
        transaction.creditor.name = Some(value.to_owned());
    } else if path_ends(path, &["CdtrAcct", "Id", "IBAN"]) {
        transaction.creditor.iban = Some(value.to_owned());
    } else if path_ends(path, &["CdtrAgt", "FinInstnId", "BICFI"]) {
        transaction.creditor.bic = Some(value.to_owned());
    } else if path_ends(path, &["Purp", "Cd"]) {
        transaction.purpose_code = Some(value.to_owned());
    } else if path_ends(path, &["Ustrd"]) {
        transaction.remittance = Some(value.to_owned());
    } else {
        assign_code(path, value, &mut transaction.code);
    }
    Ok(())
}

fn assign_code(path: &[String], value: &str, code: &mut CamtCode) {
    if path_ends(path, &["Domn", "Cd"]) {
        code.domain = Some(value.to_owned());
    } else if path_ends(path, &["Fmly", "Cd"]) {
        code.family = Some(value.to_owned());
    } else if path_ends(path, &["Fmly", "SubFmlyCd"]) {
        code.subfamily = Some(value.to_owned());
    } else if path_ends(path, &["Prtry", "Cd"]) {
        code.proprietary = Some(value.to_owned());
    } else if path_ends(path, &["Prtry", "Issr"]) {
        code.issuer = Some(value.to_owned());
    }
}

fn context(path: &[String]) -> Context {
    if path.iter().any(|name| name == "TxDtls") {
        Context::Transaction
    } else if path.iter().any(|name| name == "Ntry") {
        Context::Entry
    } else if path.iter().any(|name| name == "Bal") {
        Context::Balance
    } else {
        Context::None
    }
}

fn normalize_report(report: &mut CamtReport) {
    for entry in &mut report.entries {
        if entry.currency.is_empty() {
            entry.currency = report.account.currency.clone().unwrap_or_default();
        }
        if entry.transactions.is_empty() {
            entry.transactions.push(CamtTransaction {
                amount: entry.amount,
                currency: entry.currency.clone(),
                credit_debit: entry.credit_debit.clone(),
                end_to_end_id: None,
                transaction_at: None,
                debtor: CamtParty::default(),
                creditor: CamtParty::default(),
                purpose_code: None,
                remittance: None,
                code: entry.code.clone(),
                original_amount: None,
                original_currency: None,
            });
        }
        for transaction in &mut entry.transactions {
            if transaction.currency.is_empty() {
                transaction.currency = entry.currency.clone();
            }
            if transaction.code.domain.is_none() {
                transaction.code = entry.code.clone();
            }
        }
    }
    for balance in &mut report.balances {
        if balance.currency.is_empty() {
            balance.currency = report.account.currency.clone().unwrap_or_default();
        }
    }
}

fn path_ends(path: &[String], suffix: &[&str]) -> bool {
    path.len() >= suffix.len()
        && path[path.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(actual, expected)| actual == expected)
}

fn local_name(name: &[u8]) -> String {
    std::str::from_utf8(name)
        .unwrap_or_default()
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn attribute_value(start: &BytesStart<'_>, name: &str) -> Option<String> {
    start
        .attributes()
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()) == name)
        .and_then(|attribute| String::from_utf8(attribute.value.into_owned()).ok())
}

fn decimal(value: &str) -> Result<Decimal> {
    Decimal::from_str(&value.replace(',', "."))
        .map_err(|error| parse_error(format!("invalid CAMT amount {value:?}: {error}")))
}

fn date(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|error| parse_error(format!("invalid CAMT date {value:?}: {error}")))
}

fn parse_error(message: String) -> FinTSError {
    FinTSError::Dialog(format!("CAMT parse failed: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dkb_like_booked_report() {
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
            <Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.052.001.08">
              <BkToCstmrAcctRpt><Rpt>
                <Acct><Id><IBAN>DE001</IBAN></Id><Ccy>EUR</Ccy>
                  <Svcr><FinInstnId><BICFI>TESTBIC</BICFI></FinInstnId></Svcr>
                </Acct>
                <Bal><Tp><CdOrPrtry><Cd>CLBD</Cd></CdOrPrtry></Tp>
                  <Amt Ccy="EUR">100.00</Amt><CdtDbtInd>CRDT</CdtDbtInd>
                  <Dt><Dt>2026-07-26</Dt></Dt>
                </Bal>
                <Ntry><Amt Ccy="EUR">4.44</Amt><CdtDbtInd>DBIT</CdtDbtInd>
                  <Sts><Cd>BOOK</Cd></Sts><BookgDt><Dt>2026-07-20</Dt></BookgDt>
                  <AcctSvcrRef>ref-1</AcctSvcrRef>
                  <NtryDtls><TxDtls><Refs><EndToEndId>e2e-1</EndToEndId></Refs>
                    <Amt Ccy="EUR">4.44</Amt><CdtDbtInd>DBIT</CdtDbtInd>
                    <RltdPties><Cdtr><Pty><Nm>Merchant</Nm></Pty></Cdtr></RltdPties>
                    <Purp><Cd>IDCP</Cd></Purp><RmtInf><Ustrd>Card payment</Ustrd></RmtInf>
                  </TxDtls></NtryDtls>
                </Ntry>
              </Rpt></BkToCstmrAcctRpt>
            </Document>"#;

        let report = parse_report(xml, false).unwrap();
        assert_eq!(report.account.iban.as_deref(), Some("DE001"));
        assert_eq!(report.account.bic.as_deref(), Some("TESTBIC"));
        assert_eq!(report.balances[0].code, "CLBD");
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, "BOOK");
        assert_eq!(report.entries[0].acct_svcr_ref.as_deref(), Some("ref-1"));
        assert_eq!(
            report.entries[0].transactions[0].amount,
            Decimal::new(444, 2)
        );
        assert_eq!(
            report.entries[0].transactions[0].creditor.name.as_deref(),
            Some("Merchant")
        );
    }

    #[test]
    fn parses_pending_report_and_original_currency() {
        let xml = br#"<Document><BkToCstmrAcctRpt><Rpt><Acct><Ccy>EUR</Ccy></Acct>
            <Ntry><Amt Ccy="EUR">0.68</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts><Cd>PDNG</Cd></Sts>
              <NtryDtls><TxDtls><Amt Ccy="EUR">0.68</Amt><CdtDbtInd>DBIT</CdtDbtInd>
                <AmtDtls><InstdAmt><Amt Ccy="USD">0.77</Amt></InstdAmt></AmtDtls>
              </TxDtls></NtryDtls>
            </Ntry></Rpt></BkToCstmrAcctRpt></Document>"#;
        let report = parse_report(xml, true).unwrap();
        assert_eq!(report.entries[0].status, "PDNG");
        assert_eq!(
            report.entries[0].transactions[0]
                .original_currency
                .as_deref(),
            Some("USD")
        );
        assert_eq!(
            report.entries[0].transactions[0].original_amount,
            Some(Decimal::new(77, 2))
        );
    }

    #[test]
    fn parses_report_page_metadata() {
        let xml = br#"<Document><BkToCstmrAcctRpt>
            <GrpHdr><MsgPgntn><PgNb>2</PgNb><LastPgInd>true</LastPgInd></MsgPgntn></GrpHdr>
            <Rpt><Acct><Ccy>EUR</Ccy></Acct></Rpt>
        </BkToCstmrAcctRpt></Document>"#;
        let report = parse_report(xml, false).unwrap();
        assert_eq!(report.page_number, Some(2));
        assert_eq!(report.last_page, Some(true));
    }
}
