import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CheckoutPublic } from "../models/components/checkoutpublic.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CheckoutsClientGetRequest } from "../models/operations/checkoutsclientget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Checkout Session from Client
 *
 * @remarks
 * Get a checkout session by client secret.
 */
export declare function checkoutsClientGet(client: PolarCore, request: CheckoutsClientGetRequest, options?: RequestOptions): APIPromise<Result<CheckoutPublic, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=checkoutsClientGet.d.ts.map