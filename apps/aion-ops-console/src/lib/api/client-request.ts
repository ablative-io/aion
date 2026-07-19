import { apiErrorFromResponse, readJson } from './client-normalize';
import {
  type ApiCredentials,
  appendHeaders,
  buildScopedHeaders,
  buildUrl,
  mergeCredentials,
  stripTrailingSlash,
} from './client-transport';
import type { ApiClientOptions, FetchFn, RequestBody, RequestOptions } from './client-types';

/** Owns HTTP transport, credentials, cancellation, and response error handling for ApiClient. */
export class ApiRequestTransport {
  private readonly baseUrl: string;
  private readonly fetchImpl: FetchFn;
  private readonly credentials?: ApiCredentials;

  constructor(options: ApiClientOptions) {
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
    // Keep the default fetch bound to its realm while allowing an injected test transport.
    this.fetchImpl = options.fetchImpl ?? ((input, init) => globalThis.fetch(input, init));
    if (options.credentials !== undefined) {
      this.credentials = options.credentials;
    }
  }

  async request<T>(
    path: string,
    method: string,
    options: RequestOptions,
    body?: RequestBody
  ): Promise<T> {
    const init: RequestInit = {
      method,
      headers: buildScopedHeaders(mergeCredentials(this.credentials, options.credentials)),
      ...(options.signal !== undefined && { signal: options.signal }),
    };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }

    return this.send<T>(path, init);
  }

  async requestDeployScoped<T>(path: string, method: string, body?: RequestBody): Promise<T> {
    const init: RequestInit = { method, headers: this.buildDeployHeaders('application/json') };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }

    return this.send<T>(path, init);
  }

  requestDeployBinary<T>(path: string, archive: BodyInit): Promise<T> {
    return this.send<T>(path, {
      method: 'POST',
      headers: this.buildDeployHeaders('application/octet-stream'),
      body: archive,
    });
  }

  private buildDeployHeaders(contentType: string): Headers {
    const headers = new Headers({ 'content-type': contentType });
    appendHeaders(headers, this.credentials?.headers);
    if (this.credentials?.bearerToken !== undefined) {
      headers.set('authorization', `Bearer ${this.credentials.bearerToken}`);
    }
    if (this.credentials?.subject !== undefined) {
      headers.set('x-aion-subject', this.credentials.subject);
    }
    return headers;
  }

  private async send<T>(path: string, init: RequestInit): Promise<T> {
    const response = await this.fetchImpl(buildUrl(this.baseUrl, path), init);
    if (!response.ok) {
      const errorBody = await readJson(response).catch(() => null);
      throw apiErrorFromResponse(response.status, errorBody);
    }

    return (await readJson(response)) as T;
  }
}
