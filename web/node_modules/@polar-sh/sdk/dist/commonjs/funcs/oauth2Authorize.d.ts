import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { Oauth2AuthorizeResponseOauth2Authorize } from "../models/operations/oauth2authorize.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Authorize
 */
export declare function oauth2Authorize(client: PolarCore, options?: RequestOptions): APIPromise<Result<Oauth2AuthorizeResponseOauth2Authorize, SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=oauth2Authorize.d.ts.map