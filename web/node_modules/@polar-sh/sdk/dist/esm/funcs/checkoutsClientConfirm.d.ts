import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CheckoutPublicConfirmed } from "../models/components/checkoutpublicconfirmed.js";
import { CheckoutForbiddenError } from "../models/errors/checkoutforbiddenerror.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { PaymentError } from "../models/errors/paymenterror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CheckoutsClientConfirmRequest } from "../models/operations/checkoutsclientconfirm.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Confirm Checkout Session from Client
 *
 * @remarks
 * Confirm a checkout session by client secret.
 *
 * Orders and subscriptions will be processed.
 */
export declare function checkoutsClientConfirm(client: PolarCore, request: CheckoutsClientConfirmRequest, options?: RequestOptions): APIPromise<Result<CheckoutPublicConfirmed, PaymentError | CheckoutForbiddenError | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=checkoutsClientConfirm.d.ts.map