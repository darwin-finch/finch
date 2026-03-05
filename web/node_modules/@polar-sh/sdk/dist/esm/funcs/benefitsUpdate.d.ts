import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Benefit } from "../models/components/benefit.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { BenefitsUpdateRequest } from "../models/operations/benefitsupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Benefit
 *
 * @remarks
 * Update a benefit.
 *
 * **Scopes**: `benefits:write`
 */
export declare function benefitsUpdate(client: PolarCore, request: BenefitsUpdateRequest, options?: RequestOptions): APIPromise<Result<Benefit, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=benefitsUpdate.d.ts.map