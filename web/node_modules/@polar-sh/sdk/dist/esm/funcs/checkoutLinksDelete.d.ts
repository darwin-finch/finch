import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CheckoutLinksDeleteRequest } from "../models/operations/checkoutlinksdelete.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Delete Checkout Link
 *
 * @remarks
 * Delete a checkout link.
 *
 * **Scopes**: `checkout_links:write`
 */
export declare function checkoutLinksDelete(client: PolarCore, request: CheckoutLinksDeleteRequest, options?: RequestOptions): APIPromise<Result<void, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=checkoutLinksDelete.d.ts.map