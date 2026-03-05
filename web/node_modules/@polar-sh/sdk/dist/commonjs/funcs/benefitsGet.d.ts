import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Benefit } from "../models/components/benefit.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { BenefitsGetRequest } from "../models/operations/benefitsget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Benefit
 *
 * @remarks
 * Get a benefit by ID.
 *
 * **Scopes**: `benefits:read` `benefits:write`
 */
export declare function benefitsGet(client: PolarCore, request: BenefitsGetRequest, options?: RequestOptions): APIPromise<Result<Benefit, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=benefitsGet.d.ts.map