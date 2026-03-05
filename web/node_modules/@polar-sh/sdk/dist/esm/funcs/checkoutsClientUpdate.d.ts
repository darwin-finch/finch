import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CheckoutPublic } from "../models/components/checkoutpublic.js";
import { CheckoutForbiddenError } from "../models/errors/checkoutforbiddenerror.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CheckoutsClientUpdateRequest } from "../models/operations/checkoutsclientupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Checkout Session from Client
 *
 * @remarks
 * Update a checkout session by client secret.
 */
export declare function checkoutsClientUpdate(client: PolarCore, request: CheckoutsClientUpdateRequest, options?: RequestOptions): APIPromise<Result<CheckoutPublic, CheckoutForbiddenError | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=checkoutsClientUpdate.d.ts.map