#!/usr/bin/env ts-node

import axios from 'axios';
import * as bitcoin from 'bitcoinjs-lib';
import {
  AddressData,
  AddressTxOut,
  BlockTip,
  PaginationResponse,
} from '@titanbtcio/sdk';

// --- INTERFACES & TYPES ---

// Minimal types for electrs transactions.
interface ElectrsVout {
  scriptpubkey: string;
  value: number;
}
interface ElectrsTransaction {
  txid: string;
  vout: ElectrsVout[];
}

// UTXO types from electrs.
interface ElectrsUTXO {
  txid: string;
  vout: number;
  value: number;
}

// Response for checking output spending status.
interface TxOutSpend {
  spent: boolean;
}

// Minimal type for Maestro UTXO.
interface MaestroUTXO {
  txid: string;
  vout: number;
  satoshis: number;
  runes: { rune_id: string; amount: number }[];
}

// Minimal type for Rune response.
interface RuneResponse {
  id: string;
  block: number;
  burned: string;
  divisibility: number;
  etching: string;
  number: number;
  premine: string;
  supply: string;
  max_supply: string;
  spaced_rune: string;
  symbol?: string;
  mint?: any;
  pending_burns: string;
  pending_mints: string;
  inscription_id?: { txid: string; index: number };
  timestamp: number;
  turbo: boolean;
}

// --- CONFIGURATION ---

const INDEXER_BASE_URL = 'http://localhost:3030'; // your indexer endpoint
const ELECTRS_BASE_URL = 'https://electrs.test.arch.network'; // electrs instance
const MEMPOOL_SPACE_BASE_URL = 'https://mempool.space/testnet4/api'; // mempool.space instance

// Maestro configuration – update the URL and API key as needed.
const MAESTRO_BASE_URL = 'https://xbt-testnet.gomaestro-api.org/v0';
const MAESTRO_API_KEY = process.env.MAESTRO_API_KEY;
if (!MAESTRO_API_KEY) {
  throw new Error('MAESTRO_API_KEY is not set');
}

// Testnet network parameters for bitcoinjs-lib.
const BITCOIN_NETWORK_JS_LIB_TESTNET4 = {
  messagePrefix: '\x18Bitcoin Signed Message:\n',
  bech32: 'tb',
  bip32: {
    public: 0x043587cf,
    private: 0x04358394,
  },
  pubKeyHash: 0x6f,
  scriptHash: 0xc4,
  wif: 0x3f,
};

// --- UTILITY FUNCTIONS ---

async function fetchIndexerTip(): Promise<BlockTip> {
  const url = `${INDEXER_BASE_URL}/tip`;
  const res = await axios.get<BlockTip>(url);
  return res.data;
}

async function fetchElectrsTip(): Promise<BlockTip> {
  const heightUrl = `${ELECTRS_BASE_URL}/blocks/tip/height`;
  const hashUrl = `${ELECTRS_BASE_URL}/blocks/tip/hash`;
  const [hRes, hashRes] = await Promise.all([
    axios.get<string>(heightUrl),
    axios.get<string>(hashUrl),
  ]);
  return {
    height: parseInt(hRes.data, 10),
    hash: hashRes.data,
    is_at_tip: true,
  };
}

async function fetchIndexerBlockHashByHeight(height: number): Promise<string> {
  const url = `${INDEXER_BASE_URL}/block/${height}/hash`;
  const res = await axios.get<string>(url);
  return res.data;
}

async function fetchElectrsBlockHashByHeight(height: number): Promise<string> {
  const url = `${ELECTRS_BASE_URL}/block-height/${height}`;
  const res = await axios.get<string>(url);
  return res.data;
}

async function fetchIndexerBlockTxids(blockQuery: string): Promise<string[]> {
  const url = `${INDEXER_BASE_URL}/block/${blockQuery}/txids`;
  const res = await axios.get<string[]>(url);
  return res.data;
}

async function fetchElectrsBlockTxs(
  blockQuery: string,
): Promise<ElectrsTransaction[]> {
  const url = `${ELECTRS_BASE_URL}/internal/block/${blockQuery}/txs`;
  const res = await axios.get<ElectrsTransaction[]>(url);
  return res.data;
}

async function fetchElectrsTransaction(
  txid: string,
): Promise<ElectrsTransaction> {
  const url = `${ELECTRS_BASE_URL}/tx/${txid}`;
  const res = await axios.get<ElectrsTransaction>(url);
  return res.data;
}

async function fetchElectrsTxOutspend(
  txid: string,
  vout: number,
): Promise<TxOutSpend> {
  const url = `${ELECTRS_BASE_URL}/tx/${txid}/outspend/${vout}`;
  const res = await axios.get<TxOutSpend>(url);
  return res.data;
}

type IndexerAddress = AddressData & { outputs: AddressTxOut[] };

/** Fetches the address summary and walks every page of its outputs. */
async function fetchIndexerAddressData(
  address: string,
): Promise<IndexerAddress> {
  const summary = (
    await axios.get<AddressData>(`${INDEXER_BASE_URL}/address/${address}`)
  ).data;

  const outputs: AddressTxOut[] = [];
  let skip = 0;
  for (;;) {
    const page = (
      await axios.get<PaginationResponse<AddressTxOut>>(
        `${INDEXER_BASE_URL}/address/${address}/outputs`,
        { params: { skip, limit: 50 } },
      )
    ).data;
    if (page.items.length === 0) break;
    outputs.push(...page.items);
    skip = page.offset;
  }

  if (outputs.length !== summary.output_count) {
    console.error(
      `Address ${address}: summary counts ${summary.output_count} outputs, pages returned ${outputs.length}`,
    );
  }

  return { ...summary, outputs };
}

async function fetchElectrsAddressUTXOs(
  address: string,
): Promise<ElectrsUTXO[]> {
  const url = `${ELECTRS_BASE_URL}/address/${address}/utxo`;
  const res = await axios.get<ElectrsUTXO[]>(url);
  return res.data;
}

async function fetchMaestroUTXOs(address: string): Promise<MaestroUTXO[]> {
  const url = `${MAESTRO_BASE_URL}/mempool/addresses/${address}/utxos?order=desc`;
  const res = await axios.get<{ data: MaestroUTXO[] }>(url, {
    headers: {
      'api-key': MAESTRO_API_KEY,
      'Content-Type': 'application/json',
    },
  });
  return res.data.data;
}

async function fetchIndexerRuneId(runeId: string): Promise<RuneResponse> {
  const url = `${INDEXER_BASE_URL}/rune/${runeId}`;
  const res = await axios.get<RuneResponse>(url);
  return res.data;
}

async function fetchMempoolSpaceTxOutspend(
  txid: string,
  vout: number,
): Promise<TxOutSpend> {
  const url = `${MEMPOOL_SPACE_BASE_URL}/tx/${txid}/outspend/${vout}`;
  const res = await axios.get<TxOutSpend>(url);
  return res.data;
}

function toDecimals(amount: string, rune_entry: RuneResponse): string {
  const divisibility = rune_entry.divisibility;
  const cutoff = BigInt(10) ** BigInt(divisibility); // Use BigInt to avoid precision loss

  const whole = BigInt(amount) / cutoff;
  let fractional = BigInt(amount) % cutoff;

  if (fractional === BigInt(0)) {
    return whole.toString();
  } else {
    let width = divisibility;
    while (fractional % BigInt(10) === BigInt(0) && width > 0) {
      fractional /= BigInt(10);
      width--;
    }
    return `${whole}.${fractional.toString().padStart(width, '0')}`;
  }
}

// --- MAIN VERIFICATION ---
async function main(): Promise<void> {
  try {
    console.log('--- TIP VERIFICATION ---');
    console.log('Fetching tip from indexer...');
    const indexerTip = await fetchIndexerTip();
    console.log('Indexer tip:', indexerTip);

    console.log('Fetching tip from electrs...');
    const electrsTip = await fetchElectrsTip();
    console.log('Electrs tip:', electrsTip);

    if (indexerTip.height !== electrsTip.height) {
      throw new Error(
        `Tip height mismatch: indexer=${indexerTip.height} vs electrs=${electrsTip.height}`,
      );
    }
    if (indexerTip.hash !== electrsTip.hash) {
      throw new Error(
        `Tip blockhash mismatch: indexer=${indexerTip.hash} vs electrs=${electrsTip.hash}`,
      );
    }
    console.log('Tip verification PASSED.');

    // Determine the range: last several blocks.
    const tipHeight = indexerTip.height;
    const startHeight = tipHeight - 10;
    console.log(
      `\n--- BLOCK VERIFICATION for blocks ${startHeight} to ${tipHeight} ---`,
    );

    // Set to accumulate modified addresses.
    const modifiedAddresses = new Set<string>();

    // Loop through each block.
    for (let height = startHeight; height <= tipHeight; height++) {
      console.log(`\nVerifying block at height ${height}...`);

      // Get the block hash from both indexer and electrs.
      const [indexerBlockHash, electrsBlockHash] = await Promise.all([
        fetchIndexerBlockHashByHeight(height),
        fetchElectrsBlockHashByHeight(height),
      ]);
      if (indexerBlockHash !== electrsBlockHash) {
        throw new Error(
          `Block hash mismatch at height ${height}: indexer=${indexerBlockHash} vs electrs=${electrsBlockHash}`,
        );
      }
      console.log(`Block hash OK: ${indexerBlockHash}`);

      // Get txids for this block from both sources.
      const [indexerTxids, electrsTx] = await Promise.all([
        fetchIndexerBlockTxids(indexerBlockHash),
        fetchElectrsBlockTxs(electrsBlockHash),
      ]);

      if (indexerTxids.length !== electrsTx.length) {
        throw new Error(
          `TXID count mismatch at height ${height}: indexer has ${indexerTxids.length} vs electrs ${electrsTx.length}`,
        );
      }
      for (let i = 0; i < indexerTxids.length; i++) {
        if (indexerTxids[i] !== electrsTx[i].txid) {
          throw new Error(
            `TXID mismatch at block ${indexerBlockHash} index ${i}: indexer=${indexerTxids[i]} vs electrs=${electrsTx[i].txid}`,
          );
        }
      }
      console.log(
        `TXIDs verified for block ${indexerBlockHash} (${indexerTxids.length} transactions).`,
      );

      // For every transaction in this block, use electrs to get details.
      for (const tx of electrsTx) {
        for (let voutIndex = 0; voutIndex < tx.vout.length; voutIndex++) {
          const output = tx.vout[voutIndex];
          let address: string;
          try {
            const scriptBuffer = Buffer.from(output.scriptpubkey, 'hex');
            address = bitcoin.address.fromOutputScript(
              scriptBuffer,
              BITCOIN_NETWORK_JS_LIB_TESTNET4,
            );
          } catch (err) {
            // If decoding fails (e.g. nonstandard script), skip.
            continue;
          }
          modifiedAddresses.add(address);
        }
      }
    }

    const addressesModified = Array.from(modifiedAddresses);
    console.log(
      `\nCollected ${addressesModified.length} modified addresses from the last blocks.`,
    );

    console.log('\n--- ADDRESS OUTPUTS VERIFICATION ---');
    // For each modified address, perform checks.
    for (const address of addressesModified) {
      console.log(`Verifying address ${address}...`);
      let indexerData: IndexerAddress;
      let electrsUTXOs: ElectrsUTXO[];
      let maestroUTXOs: MaestroUTXO[];
      try {
        indexerData = await fetchIndexerAddressData(address);
        electrsUTXOs = await fetchElectrsAddressUTXOs(address);
        maestroUTXOs = await fetchMaestroUTXOs(address);
      } catch (err: any) {
        console.error(
          `Failed to fetch address data for ${address}: ${err.message}`,
        );
        continue;
      }

      for (const output of indexerData.outputs) {
        if (output.spent.spent) {
          console.error(
            `Address ${address} output ${output.txid}:${output.vout} is spent`,
          );
        }
      }

      // Normalize indexer outputs (basic fields).
      const normalizedIndexerBasic = (indexerData.outputs || []).map((o) => ({
        txid: o.txid,
        vout: Number(o.vout),
        value: o.value,
      }));

      // Normalize electrs outputs (basic fields).
      const normalizedElectrsOutputs = (electrsUTXOs || []).map((o) => ({
        txid: o.txid,
        vout: Number(o.vout),
        value: o.value,
      }));

      // Sort both arrays by txid and vout.
      const sortFunc = (
        a: { txid: string; vout: number },
        b: { txid: string; vout: number },
      ) => {
        if (a.txid < b.txid) return -1;
        if (a.txid > b.txid) return 1;
        return a.vout - b.vout;
      };
      normalizedIndexerBasic.sort(sortFunc);
      normalizedElectrsOutputs.sort(sortFunc);

      if (normalizedIndexerBasic.length !== normalizedElectrsOutputs.length) {
        if (normalizedIndexerBasic.length > normalizedElectrsOutputs.length) {
          const extraOutputs = normalizedIndexerBasic.filter(
            (o) =>
              !normalizedElectrsOutputs.some(
                (e) => e.txid === o.txid && e.vout === o.vout,
              ),
          );
          console.error(
            `Address ${address} has extra outputs: ${JSON.stringify(extraOutputs)}`,
          );
        } else {
          const missingOutputs = normalizedElectrsOutputs.filter(
            (e) =>
              !normalizedIndexerBasic.some(
                (i) => i.txid === e.txid && i.vout === e.vout,
              ),
          );

          for (const missingOutput of missingOutputs) {
            const mempoolSpaceTxOutspend = await fetchMempoolSpaceTxOutspend(
              missingOutput.txid,
              missingOutput.vout,
            );

            if (mempoolSpaceTxOutspend.spent) {
              console.error(
                `Address ${address} has missing outputs: ${JSON.stringify(missingOutput)}`,
              );
            }
          }
        }
      }

      // Compare each output’s basic fields.
      for (let i = 0; i < normalizedIndexerBasic.length; i++) {
        const idxOut = normalizedIndexerBasic[i];
        const elecOut = normalizedElectrsOutputs.find(
          (o) => o.txid === idxOut.txid && o.vout === idxOut.vout,
        );

        if (!elecOut) {
          try {
            const [mempoolSpaceTxOutspend, electrsTransaction] =
              await Promise.all([
                fetchMempoolSpaceTxOutspend(idxOut.txid, idxOut.vout),
                fetchElectrsTransaction(idxOut.txid),
              ]);

            if (mempoolSpaceTxOutspend.spent) {
              console.error(
                `Address ${address} outpoint is not in electrs: ${JSON.stringify(idxOut)}`,
              );
            }
          } catch (err) {
            console.error(
              `Address ${address} outpoint is not in electrs: ${JSON.stringify(idxOut)}`,
            );
          }
          continue;
        }

        if (
          idxOut.txid !== elecOut.txid ||
          idxOut.vout !== elecOut.vout ||
          idxOut.value !== elecOut.value
        ) {
          throw new Error(
            `Address ${address} basic output mismatch at index ${i}: indexer=${JSON.stringify(
              idxOut,
            )} vs electrs=${JSON.stringify(elecOut)}`,
          );
        }
      }

      // Now check runes arrays using Maestro.
      const normalizedMaestroOutputs = (maestroUTXOs || []).map((o) => ({
        txid: o.txid,
        vout: Number(o.vout),
        value: o.satoshis,
        runes: o.runes,
      }));

      const normalizedIndexerForRunes = (indexerData.outputs || []).map((o) => {
        const mergedRunes = o.runes;
        for (const rune of o.risky_runes) {
          const existingRune = mergedRunes.find(
            (r) => r.id === rune.id,
          );
          if (existingRune) {
            existingRune.amount = (
              BigInt(existingRune.amount) + BigInt(rune.amount)
            ).toString();
          } else {
            mergedRunes.push(rune);
          }
        }

        return {
          txid: o.txid,
          vout: Number(o.vout),
          // Indexer RuneAmount uses `id`; normalize to Maestro's `rune_id`.
          runes: mergedRunes.map((r) => ({ rune_id: r.id, amount: r.amount })),
        };
      });

      // Helper to normalize runes array.
      const normalizeRunes = (
        runes: { rune_id: string; amount: string | number }[],
      ) =>
        (runes || [])
          .map((r) => ({ rune_id: r.rune_id, amount: r.amount.toString() }))
          .sort((a, b) => a.rune_id.localeCompare(b.rune_id));

      // Collect unique rune IDs.
      const uniqueRuneIds = new Set<string>();
      for (const out of normalizedIndexerForRunes) {
        for (const rune of out.runes) {
          uniqueRuneIds.add(rune.rune_id);
        }
      }

      const runePromises: Promise<RuneResponse>[] = [];
      uniqueRuneIds.forEach((runeId) => {
        runePromises.push(fetchIndexerRuneId(runeId));
      });

      const runeIds = await Promise.all(runePromises);

      for (let i = 0; i < normalizedIndexerForRunes.length; i++) {
        const idxOut = normalizedIndexerForRunes[i];
        const maestroOut = normalizedMaestroOutputs.find(
          (o) => o.txid === idxOut.txid && o.vout === idxOut.vout,
        );

        // If Maestro did not return this output (Maestro may return only a subset), skip.
        if (!maestroOut) continue;

        const idxRunes = normalizeRunes(idxOut.runes);
        const maestroRunes = normalizeRunes(maestroOut.runes);

        if (idxRunes.length !== maestroRunes.length) {
          console.error(
            `Address ${address} output ${idxOut.txid}:${idxOut.vout} runes length mismatch: indexer has ${idxRunes.length} vs Maestro has ${maestroRunes.length}`,
          );
          console.error(`Maestro runes: ${JSON.stringify(maestroRunes)}`);
          console.error(`Indexer runes: ${JSON.stringify(idxRunes)}`);
          continue;
        }

        for (let j = 0; j < idxRunes.length; j++) {
          const idxRune = idxRunes[j];
          const runeEntry = runeIds.find((r) => r.id === idxRune.rune_id);
          if (!runeEntry) {
            throw new Error(`Rune ${idxRune.rune_id} not found`);
          }
          const maestroRune = maestroRunes.find(
            (r) => r.rune_id === idxRune.rune_id,
          );
          if (!maestroRune) {
            throw new Error(`Rune ${idxRune.rune_id} not found in Maestro`);
          }
          const maestroAmount = maestroRune.amount.toString();
          const indexerAmount = toDecimals(idxRune.amount, runeEntry);

          if (
            Number(indexerAmount) !== Number(maestroAmount) ||
            idxRune.rune_id !== maestroRune.rune_id
          ) {
            throw new Error(
              `Address ${address} output ${idxOut.txid}:${idxOut.vout} runes mismatch at index ${j}: indexer=${JSON.stringify(
                idxRune,
              )} vs Maestro=${JSON.stringify(maestroRune)}. Amounts: indexer=${indexerAmount} vs Maestro=${maestroAmount}`,
            );
          }
        }
      }
    }

    console.log('\nALL VERIFICATIONS PASSED SUCCESSFULLY.');
  } catch (err: any) {
    console.error('Verification failed:', err.message);
    process.exit(1);
  }
}

main();
