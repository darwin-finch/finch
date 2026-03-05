import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { TokenResponse } from "../models/components/tokenresponse.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { Oauth2RequestTokenRequestBody } from "../models/operations/oauth2requesttoken.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Request Token
 *
 * @remarks
 * Request an access token using a valid grant.
 */
export declare function oauth2Token(client: PolarCore, request: Oauth2RequestTokenRequestBody, options?: RequestOptions): APIPromise<Result<TokenResponse, SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=oauth2Token.d.ts.map