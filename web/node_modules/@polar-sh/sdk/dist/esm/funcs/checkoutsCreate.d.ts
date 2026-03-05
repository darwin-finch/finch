import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Checkout } from "../models/components/checkout.js";
import { CheckoutCreate } from "../models/components/checkoutcreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Checkout Session
 *
 * @remarks
 * Create a checkout session.
 *
 * **Scopes**: `checkouts:write`
 */
export declare function checkoutsCreate(client: PolarCore, request: CheckoutCreate, options?: RequestOptions): APIPromise<Result<Checkout, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=checkoutsCreate.d.ts.map