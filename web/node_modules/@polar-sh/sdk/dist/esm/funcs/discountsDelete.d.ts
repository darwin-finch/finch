import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { DiscountsDeleteRequest } from "../models/operations/discountsdelete.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Delete Discount
 *
 * @remarks
 * Delete a discount.
 *
 * **Scopes**: `discounts:write`
 */
export declare function discountsDelete(client: PolarCore, request: DiscountsDeleteRequest, options?: RequestOptions): APIPromise<Result<void, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=discountsDelete.d.ts.map