import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CheckoutLink } from "../models/components/checkoutlink.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CheckoutLinksCreateCheckoutLinkCreate } from "../models/operations/checkoutlinkscreate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Checkout Link
 *
 * @remarks
 * Create a checkout link.
 *
 * **Scopes**: `checkout_links:write`
 */
export declare function checkoutLinksCreate(client: PolarCore, request: CheckoutLinksCreateCheckoutLinkCreate, options?: RequestOptions): APIPromise<Result<CheckoutLink, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=checkoutLinksCreate.d.ts.map