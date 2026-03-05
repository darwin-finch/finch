import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { RevokeTokenRequest } from "../models/components/revoketokenrequest.js";
import { RevokeTokenResponse } from "../models/components/revoketokenresponse.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Revoke Token
 *
 * @remarks
 * Revoke an access token or a refresh token.
 */
export declare function oauth2Revoke(client: PolarCore, request: RevokeTokenRequest, options?: RequestOptions): APIPromise<Result<RevokeTokenResponse, SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=oauth2Revoke.d.ts.map