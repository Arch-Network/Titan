import axios, { AxiosInstance, AxiosError, AxiosRequestConfig } from 'axios';
import {
  AddressData,
  AddressTxOut,
  BlockTip,
  Pagination,
  PaginationResponse,
  RuneResponse,
  Subscription,
  Status,
  Transaction,
  TxOutEntry,
  TransactionStatus,
  Block,
  MempoolEntry,
  MempoolTxEntry,
} from './types';

export interface TitanHttpClientOptions {
  /**
   * Admin token for webhook subscription endpoints, sent as
   * `Authorization: Bearer <token>`.
   */
  adminToken?: string;
}

/**
 * A HTTP client class for the Titan API. Each method corresponds
 * to an endpoint (for example: GET /status, GET /tx/:txid, POST /tx/broadcast, etc.)
 */
export class TitanHttpClient {
  private http: AxiosInstance;
  private baseUrl: string;
  private adminToken?: string;

  constructor(baseUrl: string, options: TitanHttpClientOptions = {}) {
    this.adminToken = options.adminToken;
    // Remove any trailing slashes from the URL.
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.http = axios.create({
      baseURL: this.baseUrl,
    });
  }

  async getStatus(): Promise<Status> {
    return await this.getOrFail<Status>('/status');
  }

  async getTip(): Promise<BlockTip> {
    return await this.getOrFail<BlockTip>('/tip');
  }

  /**
   * Fetches a block by its query (could be a block height or hash).
   */
  async getBlock(query: string): Promise<Block | undefined> {
    return await this.get<Block>(`/block/${query}`);
  }

  async getBlockHashByHeight(height: number): Promise<string | undefined> {
    return await this.get<string>(`/block/${height}/hash`);
  }

  async getBlockTxids(query: string): Promise<string[] | undefined> {
    return await this.get<string[]>(`/block/${query}/txids`);
  }

  /** Balance summary of an address (value, runes and output count). */
  async getAddress(address: string): Promise<AddressData> {
    return await this.getOrFail<AddressData>(`/address/${address}`);
  }

  /**
   * One page of an address's outputs (at most 50). Pass the returned `offset` as the
   * next `skip`; an empty page means the end.
   */
  async getAddressOutputs(
    address: string,
    pagination?: Pagination,
  ): Promise<PaginationResponse<AddressTxOut>> {
    return await this.getOrFail<PaginationResponse<AddressTxOut>>(
      `/address/${address}/outputs`,
      { params: pagination || {} },
    );
  }

  async getTransaction(txid: string): Promise<Transaction | undefined> {
    return await this.get<Transaction>(`/tx/${txid}`);
  }

  async getTransactionRaw(txid: string): Promise<Uint8Array | undefined> {
    // Request raw binary data using the arraybuffer responseType.
    const response = await this.get<ArrayBuffer>(`/tx/${txid}/raw`, {
      responseType: 'arraybuffer',
    });

    if (!response) {
      return undefined;
    }

    return new Uint8Array(response);
  }

  async getTransactionHex(txid: string): Promise<string | undefined> {
    return await this.get<string>(`/tx/${txid}/hex`);
  }

  async getTransactionStatus(
    txid: string,
  ): Promise<TransactionStatus | undefined> {
    return await this.get<TransactionStatus>(`/tx/${txid}/status`);
  }

  async sendTransaction(txHex: string): Promise<string> {
    try {
      const response = await this.http.post<string>('/tx/broadcast', txHex, {
        headers: {
          'Content-Type': 'text/plain',
        },
      });

      return response.data;
    } catch (err) {
      if (err instanceof AxiosError) {
        const axiosError = err as AxiosError;
        if (axiosError.response?.data) {
          const error = axiosError.response.data as any;
          throw error;
        }
      }

      throw err;
    }
  }

  async getOutput(txid: string, vout: number): Promise<TxOutEntry | undefined> {
    return await this.get<TxOutEntry>(`/output/${txid}:${vout}`);
  }

  async getInscription(
    inscriptionId: string,
  ): Promise<{ headers: any; data: Uint8Array }> {
    const response = await this.http.get<ArrayBuffer>(
      `/inscription/${inscriptionId}`,
      {
        responseType: 'arraybuffer',
      },
    );

    return {
      headers: response.headers,
      data: new Uint8Array(response.data),
    };
  }

  async getRunes(
    pagination?: Pagination,
  ): Promise<PaginationResponse<RuneResponse>> {
    const params = pagination || {};
    return await this.getOrFail<PaginationResponse<RuneResponse>>('/runes', {
      params,
    });
  }

  async getRune(rune: string): Promise<RuneResponse | undefined> {
    return await this.get<RuneResponse>(`/rune/${rune}`);
  }

  async getRuneTransactions(
    rune: string,
    pagination?: Pagination,
  ): Promise<PaginationResponse<string> | undefined> {
    const params = pagination || {};
    return await this.getOrFail<PaginationResponse<string>>(
      `/rune/${rune}/transactions`,
      { params },
    );
  }

  /** One page of mempool txids in txid byte order (at most 1000). */
  async getMempoolTxids(
    pagination?: Pagination,
  ): Promise<PaginationResponse<string>> {
    return await this.getOrFail<PaginationResponse<string>>(`/mempool/txids`, {
      params: pagination || {},
    });
  }

  async getMempoolEntry(txid: string): Promise<MempoolEntry | undefined> {
    return await this.get<MempoolEntry>(`/mempool/entry/${txid}`);
  }

  async getMempoolEntries(
    txids: string[],
  ): Promise<Map<string, MempoolEntry | undefined>> {
    const response = await this.http.post<
      Record<string, MempoolEntry | undefined>
    >('/mempool/entries', txids);

    return new Map(Object.entries(response.data));
  }

  async getMempoolEntriesWithAncestors(
    txids: string[],
  ): Promise<Map<string, MempoolEntry>> {
    const response = await this.http.post<Record<string, MempoolEntry>>(
      '/mempool/entries/ancestors',
      txids,
    );
    return new Map(Object.entries(response.data));
  }

  /** One page of mempool entries in txid byte order (at most 50). */
  async listMempoolEntries(
    pagination?: Pagination,
  ): Promise<PaginationResponse<MempoolTxEntry>> {
    return await this.getOrFail<PaginationResponse<MempoolTxEntry>>(
      '/mempool/entries',
      { params: pagination || {} },
    );
  }

  async getSubscription(id: string): Promise<Subscription | undefined> {
    return await this.get<Subscription>(
      `/subscription/${id}`,
      this.adminConfig(),
    );
  }

  async listSubscriptions(): Promise<Subscription[]> {
    return await this.getOrFail<Subscription[]>(
      '/subscriptions',
      this.adminConfig(),
    );
  }

  async addSubscription(subscription: Subscription): Promise<Subscription> {
    const response = await this.http.post<Subscription>(
      '/subscription',
      subscription,
      this.adminConfig(),
    );
    return response.data;
  }

  async deleteSubscription(id: string): Promise<void> {
    const response = await this.http.delete(
      `/subscription/${id}`,
      this.adminConfig(),
    );
    if (response.status < 200 || response.status >= 300) {
      throw new Error(`Delete subscription failed: HTTP ${response.status}`);
    }
  }

  private adminConfig(): AxiosRequestConfig | undefined {
    return this.adminToken
      ? { headers: { Authorization: `Bearer ${this.adminToken}` } }
      : undefined;
  }

  private async getOrFail<T>(
    path: string,
    config?: AxiosRequestConfig,
  ): Promise<T> {
    const response = await this.http.get<T>(path, config);
    return response.data;
  }

  private async get<T>(
    path: string,
    config?: AxiosRequestConfig,
  ): Promise<T | undefined> {
    try {
      const response = await this.http.get<T>(path, config);
      return response.data;
    } catch (err) {
      if (err instanceof AxiosError) {
        const axiosError = err as AxiosError;
        if (axiosError.response?.status === 404) {
          return undefined;
        }
      }

      throw err;
    }
  }
}
