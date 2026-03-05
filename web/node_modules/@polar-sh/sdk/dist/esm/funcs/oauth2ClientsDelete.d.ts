import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { Oauth2ClientsOauth2DeleteClientRequest } from "../models/operations/oauth2clientsoauth2deleteclient.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Delete Client
 *
 * @remarks
 * Delete an OAuth2 client.
 */
export declare function oauth2ClientsDelete(client: PolarCore, request: Oauth2ClientsOauth2DeleteClientRequest, options?: RequestOptions): APIPromise<Result<any, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=oauth2ClientsDelete.d.ts.map