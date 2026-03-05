import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { BenefitsDeleteRequest } from "../models/operations/benefitsdelete.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Delete Benefit
 *
 * @remarks
 * Delete a benefit.
 *
 * > [!WARNING]
 * > Every grants associated with the benefit will be revoked.
 * > Users will lose access to the benefit.
 *
 * **Scopes**: `benefits:write`
 */
export declare function benefitsDelete(client: PolarCore, request: BenefitsDeleteRequest, options?: RequestOptions): APIPromise<Result<void, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=benefitsDelete.d.ts.map