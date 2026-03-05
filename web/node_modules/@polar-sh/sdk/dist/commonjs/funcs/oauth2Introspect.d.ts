import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { IntrospectTokenRequest } from "../models/components/introspecttokenrequest.js";
import { IntrospectTokenResponse } from "../models/components/introspecttokenresponse.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Introspect Token
 *
 * @remarks
 * Get information about an access token.
 */
export declare function oauth2Introspect(client: PolarCore, request: IntrospectTokenRequest, options?: RequestOptions): APIPromise<Result<IntrospectTokenResponse, SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=oauth2Introspect.d.ts.map